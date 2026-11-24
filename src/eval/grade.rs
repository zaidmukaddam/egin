//! Graders beyond `expect`/`check`: a set-valued final answer (GraphWalks) and a model judge for
//! short factual answers (SimpleQA).

use crate::{model, App};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;

/// The node set on the last line of the form `Final Answer: [a, b]`, as GraphWalks asks for.
/// Anything else is no answer, which scores as a miss (the official grader does the same).
/// One exception: a list that runs to the end of the reply may lack its `]`, because the
/// router's stream drops a trailing `]` from minimax-m3 after tool calls.
pub fn final_set(answer: &str) -> Option<BTreeSet<String>> {
    let last = answer.trim_end().lines().last()?;
    let list = last.split_once("Final Answer:")?.1.trim();
    let inner = list.strip_prefix('[')?;
    let inner = inner.strip_suffix(']').unwrap_or(inner);
    if inner.contains(']') {
        return None;
    }
    Some(inner.split(',').map(|s| s.trim().trim_matches(['\'', '"', '`']).to_string()).filter(|s| !s.is_empty()).collect())
}

/// GAIA's official scorer on the text after the reply's last `FINAL ANSWER:`. A numeric gold
/// answer compares as a number (ignoring `$`, `%` and `,`), a gold answer with `,` or `;` as a
/// list element by element, anything else as a string without whitespace, case or punctuation.
pub fn gaia(reply: &str, gold: &str) -> bool {
    let Some((_, answer)) = reply.rsplit_once("FINAL ANSWER:") else { return false };
    let number = |s: &str| s.replace(['$', '%', ','], "").trim().parse::<f64>().ok();
    let text = |s: &str, strip_punct: bool| -> String {
        s.chars().filter(|c| !(c.is_whitespace() || strip_punct && c.is_ascii_punctuation())).flat_map(char::to_lowercase).collect()
    };
    let (answer, gold) = (answer.trim(), gold.trim());
    if let Ok(want) = gold.parse::<f64>() {
        return number(answer) == Some(want);
    }
    if gold.contains([',', ';']) {
        let (got, want): (Vec<&str>, Vec<&str>) = (answer.split([',', ';']).collect(), gold.split([',', ';']).collect());
        return got.len() == want.len()
            && got.iter().zip(&want).all(|(g, w)| match w.trim().parse::<f64>() {
                Ok(w) => number(g) == Some(w),
                Err(_) => text(g, false) == text(w, false),
            });
    }
    text(answer, true) == text(gold, true)
}

/// A judged short answer: SimpleQA's three grades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Correct,
    Incorrect,
    NotAttempted,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judge {
    /// The gold answer.
    pub answer: String,
    /// The judge model, pinned per task so scores stay comparable across runs.
    pub model: String,
}

const JUDGE_PROMPT: &str = "You grade answers to short factual questions against a gold target. Reply with one letter.\n\
A = CORRECT: the response states the gold target's meaning, with no contradicting claim. Extra detail, \
paraphrase, different capitalisation or punctuation, and small spelling slips in names are fine. A number \
is correct when it matches the gold target to its last significant figure.\n\
B = INCORRECT: the response states an answer that contradicts the gold target, or hedges between \
several candidates where any is wrong.\n\
C = NOT_ATTEMPTED: the response gives no answer to the question (for example it says it does not know), \
and contradicts nothing.\n\
Judge only the final answer, not the reasoning. Do not use your own knowledge of the question.";

impl Judge {
    pub async fn grade(&self, app: &App, question: &str, response: &str) -> Result<Grade> {
        let user = format!("Question: {question}\nGold target: {}\nResponse: {response}\n\nGrade (A, B or C):", self.answer);
        let body = json!({
            "model": self.model,
            "messages": [{"role": "system", "content": JUDGE_PROMPT}, {"role": "user", "content": user}],
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        let ep = model::Endpoint::resolve(app, &self.model, None)?;
        let reply = model::chat(app, &ep, &body, &mut |_, _| {}).await.context("judge call")?;
        let text = reply.message["content"].as_str().unwrap_or("");
        Ok(match text.trim().chars().find(|c| matches!(c, 'A' | 'B' | 'C')) {
            Some('A') => Grade::Correct,
            Some('C') => Grade::NotAttempted,
            _ => Grade::Incorrect,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{final_set, gaia};

    #[test]
    fn gaia_scores_numbers_lists_and_strings() {
        assert!(gaia("thinking...\nFINAL ANSWER: $1,234.50", "1234.5"));
        assert!(!gaia("FINAL ANSWER: 1235", "1234.5"));
        assert!(gaia("FINAL ANSWER: Egalitarian.", "egalitarian"));
        assert!(gaia("FINAL ANSWER: sea gull", "Seagull"));
        assert!(gaia("FINAL ANSWER: apple, Pear;  3", "apple, pear, 3"));
        assert!(!gaia("FINAL ANSWER: apple, pear", "apple, pear, 3"), "list length must match");
        assert!(!gaia("the answer is egalitarian", "egalitarian"), "no FINAL ANSWER line is a miss");
        assert!(gaia("FINAL ANSWER: wrong\nwait.\nFINAL ANSWER: right", "right"), "the last one counts");
    }

    fn set(items: &[&str]) -> Option<std::collections::BTreeSet<String>> {
        Some(items.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn final_set_reads_only_the_last_line() {
        assert_eq!(final_set("step 1\nFinal Answer: [b2, a1]"), set(&["a1", "b2"]));
        assert_eq!(final_set("Final Answer: ['a1', \"b2\"]\n"), set(&["a1", "b2"]));
        assert_eq!(final_set("Final Answer: []"), set(&[]));
        assert_eq!(final_set("Final Answer: [a1]\nhope that helps"), None);
        assert_eq!(final_set("Final Answer: [a1] is my guess"), None);
        assert_eq!(final_set("Final Answer: [b2, a1"), set(&["a1", "b2"]), "a dropped closing bracket still counts");
        assert_eq!(final_set("Final Answer: ["), set(&[]));
        assert_eq!(final_set("the nodes are a1 and b2"), None);
    }
}
