//! The eval runner (variants, fresh workspaces, traces, splits) and the results page.

mod common;

use common::{bash, fx, say};
use egin::{eval, Approve};

#[tokio::test]
async fn eval_runner_scores_every_task_under_every_variant() {
    // Variants run in name order (bash-only, default); tasks in file order.
    let one_variant = vec![bash("c1", "printf egin > hello.txt"), say("created"), say("Rust 1.0 shipped in 2015.")];
    let f = fx([one_variant.clone(), one_variant].concat(), Approve::Auto).await;
    // One job at a time so the scripted replies line up with the runs.
    let opts = eval::Opts { jobs: 1, ..Default::default() };
    let outcomes = eval::run_evals(&f.app, &f.dir.canonicalize().unwrap(), &opts).await.unwrap();
    let got: Vec<_> = outcomes.iter().map(|o| (o.eval.as_str(), o.variant.as_str(), o.pass, o.turns)).collect();
    assert_eq!(
        got,
        [("file-task", "bash-only", true, 2), ("web-lookup", "bash-only", true, 1), ("file-task", "default", true, 2), ("web-lookup", "default", true, 1)]
    );
    assert!(outcomes.iter().all(|o| o.cost > 0.0 && o.prompt_tokens > 0));
    let has_write = |req: usize| f.mock.reqs()[req]["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "write_file");
    assert!(!has_write(0) && has_write(3), "the variant's overrides reached the agent");
    assert!(!f.dir.join("workspace/hello.txt").exists(), "evals run in a scratch copy of the workspace");
    assert_eq!(std::fs::read_dir(f.dir.join("evals/results")).unwrap().count(), 1);

    // A wrong answer fails.
    let f = fx(vec![say("no file made"), say("1999")], Approve::Auto).await;
    let opts = eval::Opts { jobs: 1, variants: vec!["default".into()], ..Default::default() };
    let outcomes = eval::run_evals(&f.app, &f.dir.canonicalize().unwrap(), &opts).await.unwrap();
    assert_eq!(outcomes.iter().map(|o| o.pass).collect::<Vec<_>>(), [false, false]);
    // Every run leaves a readable trace.
    let t = std::path::Path::new(&outcomes[0].trace);
    assert!(std::fs::read_to_string(t.join("transcript.md")).unwrap().contains("## assistant\n\nno file made"));
    assert!(t.join("messages.json").exists() && t.join("events.jsonl").exists());

    // Folder tasks bring their own files; splits filter.
    let f = fx(vec![say("the code is 7731")], Approve::Auto).await;
    f.write("evals/secret/task.toml", "prompt = \"read code.txt\"\nexpect = \"7731\"\nsplit = \"heldout\"\n", false);
    f.write("evals/secret/files/code.txt", "7731", false);
    let opts = eval::Opts { jobs: 1, variants: vec!["default".into()], split: Some("heldout".into()), ..Default::default() };
    let outcomes = eval::run_evals(&f.app, &f.dir.canonicalize().unwrap(), &opts).await.unwrap();
    assert_eq!(outcomes.iter().map(|o| (o.eval.as_str(), o.split.as_str(), o.pass)).collect::<Vec<_>>(), [("secret", "heldout", true)]);
}

#[tokio::test]
async fn set_answers_and_judged_answers_are_graded() {
    // Tasks run in name order: gw (set answer), qa (judged). The judge call is the reply after qa's.
    let script = vec![say("walk...\nFinal Answer: [y, x]"), say("It is Paris."), say("A"), say("Final Answer: [x]"), say("not sure"), say("C")];
    let f = fx(script, Approve::Auto).await;
    f.write("evals/gw.toml", "prompt = \"graph\"\nanswer_set = [\"x\", \"y\"]\n", false);
    f.write("evals/qa.toml", "prompt = \"Capital of France?\"\njudge = { answer = \"Paris\", model = \"mock\" }\n", false);
    let opts = eval::Opts { jobs: 1, variants: vec!["default".into()], only: vec!["gw".into(), "qa".into()], ..Default::default() };
    let dir = f.dir.canonicalize().unwrap();
    let pass = |o: &[eval::Outcome]| o.iter().map(|o| (o.eval.clone(), o.pass)).collect::<Vec<_>>();
    assert_eq!(pass(&eval::run_evals(&f.app, &dir, &opts).await.unwrap()), [("gw".into(), true), ("qa".into(), true)]);
    let judge_req = &f.mock.reqs()[2];
    assert!(judge_req["messages"][1]["content"].as_str().unwrap().contains("Gold target: Paris\nResponse: It is Paris."));
    assert_eq!(pass(&eval::run_evals(&f.app, &dir, &opts).await.unwrap()), [("gw".into(), false), ("qa".into(), false)]);
}

#[test]
fn report_page_aggregates_arms() {
    let o = |eval: &str, variant: &str, pass: bool, category: &str| eval::Outcome {
        eval: eval.into(),
        variant: variant.into(),
        model: "m".into(),
        split: String::new(),
        category: category.into(),
        pass,
        turns: 2,
        tool_calls: 1,
        prompt_tokens: 100,
        cached_tokens: 50,
        completion_tokens: 10,
        cost: 0.01,
        wall_ms: 2000,
        error: None,
        trace: String::new(),
    };
    let runs = [o("a", "egin", true, ""), o("b", "egin", false, ""), o("a", "baseline", true, ""), o("c", "egin", false, "env_error")];
    let html = eval::report::html("t", "method <b>", &runs);
    assert!(html.contains("<b>50%</b> <span class=dim>(1/2)</span>") && html.contains("<b>100%</b>"));
    assert!(html.contains("Excluded for every arm (environment did not build): c") && html.contains("method &lt;b&gt;"));
}
