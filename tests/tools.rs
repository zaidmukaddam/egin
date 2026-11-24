//! Built-in tools: bash (timeouts, background jobs), file tools, memory, the file index,
//! skills and subagents, web tools, and MCP connections (listed and deferred).

mod common;

use common::{bash, call, fx, say, MCP_SERVER};
use egin::Approve;
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::test]
async fn local_commands_do_not_see_egins_credentials() {
    std::env::set_var("YDC_API_KEY", "router-secret");
    std::env::set_var("EGIN_TEST_PLAIN_VAR", "visible");
    let f = fx(vec![bash("c1", "echo ${YDC_API_KEY:-unset} $EGIN_TEST_PLAIN_VAR"), say("ok")], Approve::Auto).await;
    let (sid, _) = f.go(&f.agent(""), "go").await;
    assert_eq!(f.tool_outputs(&sid)[0], "unset visible\n");
}

#[tokio::test]
async fn a_timeout_kills_the_whole_process_tree() {
    let marker = format!("sleep 31.{}", std::process::id());
    let f = fx(vec![call(&[("c1", "bash", json!({"command": format!("{marker} & {marker}"), "timeout_secs": 1}))]), say("ok")], Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("bash_background = false"), "go").await;
    assert_eq!(f.tool_outputs(&sid)[0], "error: timed out after 1s");
    std::thread::sleep(Duration::from_millis(200));
    let left = std::process::Command::new("pgrep").args(["-f", &marker]).output().unwrap();
    assert!(left.stdout.is_empty(), "background child survived the timeout");
}

#[tokio::test]
async fn a_slow_command_moves_to_the_background_and_can_be_checked() {
    let script = vec![call(&[("c1", "bash", json!({"command": "echo started; sleep 2; echo finished; exit 3", "timeout_secs": 1}))]), say("waiting")];
    let f = fx(script, Approve::Auto).await;
    let t = Instant::now();
    let (sid, _) = f.go(&f.agent(""), "go").await;
    assert!(t.elapsed() < Duration::from_millis(1900), "the call returned at its wait, not when the command ended");
    let out = &f.tool_outputs(&sid)[0];
    assert!(out.starts_with("started\n") && out.contains("[still running in the background as process group"), "{out}");
    let dir = out.split("full output goes to ").nth(1).unwrap().split("/stdout").next().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(1600));
    assert_eq!(std::fs::read_to_string(format!("{dir}/stdout")).unwrap(), "started\nfinished\n");
    assert_eq!(std::fs::read_to_string(format!("{dir}/exit")).unwrap().trim(), "3");

    // A fast command looks exactly like a plain run, exit code included.
    let f = fx(vec![bash("c1", "echo hi; echo err >&2; exit 2"), say("ok")], Approve::Auto).await;
    let (sid, _) = f.go(&f.agent(""), "go").await;
    assert_eq!(f.tool_outputs(&sid)[0], "hi\nerr\n\n[exit 2]");
}

#[tokio::test]
async fn write_file_needs_a_fresh_read_to_overwrite() {
    let w = |id: &str, content: &str| call(&[(id, "write_file", json!({"path": "notes.txt", "content": content}))]);
    let script = vec![
        w("c1", "new file"), // new: allowed
        w("c2", "second"),   // wrote it ourselves: allowed
        bash("c3", "echo changed-by-bash > notes.txt"),
        w("c4", "third"), // changed behind our back: refused
        call(&[("c5", "read_file", json!({"path": "/workspace-alias-unused"}))]),
        call(&[("c6", "read_file", json!({"path": "notes.txt"}))]),
        w("c7", "fourth"), // fresh read: allowed
        bash("c8", "echo other > other.txt"),
        call(&[("c9", "write_file", json!({"path": "other.txt", "content": "x"}))]), // never read: refused
        say("ok"),
    ];
    let f = fx(script, Approve::Auto).await;
    let a = f.agent("");
    let (sid, _) = f.go(&a, "go").await;
    let outs = f.tool_outputs(&sid);
    assert_eq!(outs[0], "wrote 8 bytes");
    assert_eq!(outs[1], "wrote 6 bytes");
    assert!(outs[3].contains("changed since you last read it"), "{}", outs[3]);
    assert_eq!(outs[6], "wrote 6 bytes");
    assert!(outs[8].contains("you have not read it in this session"), "{}", outs[8]);
    assert_eq!(std::fs::read_to_string(a.workspace.join("notes.txt")).unwrap(), "fourth");

    // Off, it overwrites anything.
    let script = vec![bash("c1", "echo x > o.txt"), call(&[("c2", "write_file", json!({"path": "o.txt", "content": "y"}))]), say("ok")];
    let f = fx(script, Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("read_before_write = false"), "go").await;
    assert_eq!(f.tool_outputs(&sid)[1], "wrote 1 bytes");
}

#[tokio::test]
async fn memory_is_written_capped_and_read_next_session() {
    let script = vec![
        call(&[("c1", "memory", json!({"action": "add", "text": "deploys happen on fridays"}))]),
        call(&[("c2", "memory", json!({"action": "add", "text": "x".repeat(500)}))]),
        say("ok"),
    ];
    let f = fx(script, Approve::Auto).await;
    std::fs::write(f.dir.join("memory/notes.md"), "").unwrap();
    let (sid, _) = f.go(&f.agent("memory_max = 200"), "go").await;
    let outs = f.tool_outputs(&sid);
    assert!(outs[0].starts_with("saved ("), "{}", outs[0]);
    assert!(outs[1].contains("over the 200-byte cap"), "{}", outs[1]);
    assert_eq!(std::fs::read_to_string(f.dir.join("memory/notes.md")).unwrap(), "- deploys happen on fridays\n");
    assert!(f.agent("memory_max = 200").system_prompt().contains("deploys happen on fridays"));
}

#[tokio::test]
async fn search_files_indexes_the_workspace_incrementally() {
    let search = |q: &str| call(&[("c1", "search_files", json!({"query": q}))]);
    let f = fx(vec![search("invoice overdue"), say("ok"), search("invoice"), say("ok")], Approve::Auto).await;
    let a = f.agent("index = true");
    std::fs::create_dir_all(a.workspace.join("docs")).unwrap();
    std::fs::write(a.workspace.join("docs/billing.md"), "# Billing policy\nAn invoice is overdue after 30 days.").unwrap();
    std::fs::write(a.workspace.join("docs/travel.md"), "# Travel\nBook flights two weeks ahead.").unwrap();
    let (sid, _) = f.go(&a, "go").await;
    let out = &f.tool_outputs(&sid)[0];
    assert!(out.starts_with("docs/billing.md (") && out.contains("Billing policy") && out.contains("[invoice]") && !out.contains("travel.md"), "{out}");
    assert!(out.ends_with("(2 files indexed)"));
    std::fs::remove_file(a.workspace.join("docs/billing.md")).unwrap();
    let (sid, _) = f.go(&a, "go").await;
    assert_eq!(f.tool_outputs(&sid)[0], "no matches (1 files indexed)");
}

#[tokio::test]
async fn skills_load_and_subagents_answer() {
    let script = vec![
        call(&[("c1", "skill", json!({"name": "report"}))]),
        call(&[("c2", "task", json!({"agent": "researcher", "prompt": "dig"}))]),
        say("sub answer"),
        say("final"),
    ];
    let f = fx(script, Approve::Auto).await;
    f.write("subagents/researcher/instructions.md", "You research.", false);
    f.write("subagents/researcher/agent.toml", "model = \"mock\"\napproval = \"never\"\nsandbox = \"local\"\n", false);
    let (sid, out) = f.go(&f.agent(""), "go").await;
    assert_eq!(out.unwrap(), "final");
    let outs = f.tool_outputs(&sid);
    assert!(outs[0].starts_with("Lead with the answer."));
    assert_eq!(outs[1], "sub answer");
    let sub = &f.mock.reqs()[2]["messages"];
    assert!(sub[0]["content"].as_str().unwrap().starts_with("You research."));
    assert_eq!(sub[1]["content"], "dig");
}

#[tokio::test]
async fn web_tools_work_client_side_and_router_side() {
    let script = vec![
        call(&[("c1", "web_search", json!({"query": "q"})), ("c2", "web_fetch", json!({"url": "https://example.com"}))]),
        say("ok"),
        // A router turn that mixed a web call with a function call hands both back.
        call(&[("c3", "you_web_search", json!({"query": "q"})), ("c4", "bash", json!({"command": "echo x"}))]),
        say("ok"),
    ];
    let f = fx(script, Approve::Auto).await;
    let (sid, _) = f.go(&f.agent(""), "go").await;
    assert_eq!(f.tool_outputs(&sid), ["[1] Mock Result\nhttps://example.com\nmock highlight\n\n", "# Page\nhttps://example.com\n\npage body"]);

    let (sid, _) = f.go(&f.agent("web = \"server\"\nfreshness = \"week\""), "go").await;
    let tools = f.mock.reqs()[2]["tools"].clone();
    let tools = tools.as_array().unwrap();
    assert!(
        tools.contains(&json!({"type": "web_search", "count": 5, "freshness": "week"}))
            && tools.contains(&json!({"type": "web_extract", "formats": ["markdown"]}))
    );
    assert!(!tools.iter().any(|t| t["function"]["name"] == "web_search"));
    assert_eq!(f.tool_outputs(&sid), ["[1] Mock Result\nhttps://example.com\nmock highlight\n\n", "x\n"]);
}

#[tokio::test]
async fn results_mentioning_excluded_terms_are_withheld() {
    let script = vec![call(&[("c1", "web_search", json!({"query": "q"})), ("c2", "web_fetch", json!({"url": "https://example.com"}))]), say("ok")];
    let f = fx(script, Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("exclude_terms = [\"mock highlight\", \"page body\"]"), "go").await;
    assert_eq!(f.tool_outputs(&sid), ["no results", "https://example.com\n(page withheld: it mentions a term excluded for this agent)"]);
    // Terms are case-sensitive: "Gaia" the satellite is not "GAIA" the benchmark.
    let script = vec![call(&[("c1", "web_search", json!({"query": "q"}))]), say("ok")];
    let f = fx(script, Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("exclude_terms = [\"MOCK HIGHLIGHT\"]"), "go").await;
    assert_eq!(f.tool_outputs(&sid), ["[1] Mock Result\nhttps://example.com\nmock highlight\n\n"]);
}

#[tokio::test]
async fn excluded_domains_never_reach_the_model() {
    // The mock search returns example.com whatever exclude_domains says, as a leaky API would.
    let script = vec![
        call(&[
            ("c1", "web_search", json!({"query": "q"})),
            ("c2", "web_fetch", json!({"url": "https://www.EXAMPLE.com/answers"})),
            ("c3", "web_fetch", json!({"url": "https://notexample.com/"})),
        ]),
        say("ok"),
    ];
    let f = fx(script, Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("exclude_domains = [\"example.com\"]"), "go").await;
    assert_eq!(
        f.tool_outputs(&sid),
        [
            "no results",
            "error: https://www.EXAMPLE.com/answers is blocked by this agent's exclude_domains",
            // A different domain that merely ends in the same letters is not blocked.
            "# Page\nhttps://example.com\n\npage body"
        ]
    );
}

#[tokio::test]
async fn mcp_servers_become_tools() {
    let f = fx(vec![call(&[("c1", "calc__add", json!({"a": 2, "b": 40}))]), say("ok")], Approve::Api).await;
    f.write("mcp/calc.py", MCP_SERVER, true);
    f.write("connections/calc.toml", &format!("command = [\"python3\", \"{}\"]\nsafe = [\"add\"]\n", f.dir.join("mcp/calc.py").display()), false);
    let (sid, out) = f.go(&f.agent("approval = \"ask\""), "go").await;
    out.unwrap();
    assert_eq!(f.tool_outputs(&sid), ["42"], "safe MCP tools need no approval");
    let tools = f.mock.reqs()[0]["tools"].clone();
    assert!(tools.as_array().unwrap().iter().any(|t| t["function"]["name"] == "calc__add" && t["function"]["description"] == "Add two numbers"));
}

#[tokio::test]
async fn deferred_mcp_tools_are_found_with_tool_search() {
    let script = vec![
        call(&[("c1", "tool_search", json!({"query": "add numbers"}))]),
        call(&[("c2", "tool_call", json!({"name": "calc__add", "input": {"a": 1, "b": 2}}))]),
        say("ok"),
    ];
    let f = fx(script, Approve::Api).await;
    f.write("mcp/calc.py", MCP_SERVER, true);
    f.write("connections/calc.toml", &format!("command = [\"python3\", \"{}\"]\nsafe = [\"add\"]\n", f.dir.join("mcp/calc.py").display()), false);
    let (sid, out) = f.go(&f.agent("mcp_tools = \"deferred\"\napproval = \"ask\""), "go").await;
    out.unwrap();
    let outs = f.tool_outputs(&sid);
    assert!(outs[0].starts_with(r#"{"description":"Add two numbers","name":"calc__add""#), "{}", outs[0]);
    assert_eq!(outs[1], "3", "a safe tool through tool_call needs no approval");
    let names: Vec<String> = f.mock.reqs()[0]["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"tool_search".into()) && names.contains(&"tool_call".into()) && !names.contains(&"calc__add".into()), "{names:?}");

    // An unknown name is an error the model can recover from; auto lists a small server's tools directly.
    let f = fx(vec![call(&[("c1", "tool_call", json!({"name": "calc__nope"}))]), say("ok")], Approve::Auto).await;
    f.write("mcp/calc.py", MCP_SERVER, true);
    f.write("connections/calc.toml", &format!("command = [\"python3\", \"{}\"]\n", f.dir.join("mcp/calc.py").display()), false);
    let (sid, _) = f.go(&f.agent(""), "go").await;
    assert!(f.tool_outputs(&sid)[0].contains("no connected tool named \"calc__nope\""));
    assert!(f.mock.reqs()[0]["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "calc__add"));
}

/// A .docx with one paragraph, built in memory.
fn docx(text: &str) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    z.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
    write!(z, "<w:document><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>").unwrap();
    z.finish().unwrap();
    buf.into_inner()
}

#[tokio::test]
async fn read_file_shows_images_and_turns_documents_into_text() {
    let reads = [
        ("c1", "read_file", json!({"path": "chart.png"})),
        ("c2", "read_file", json!({"path": "brief.docx"})),
        ("c3", "read_file", json!({"path": "blob.bin"})),
    ];
    let f = fx(vec![call(&reads), say("done")], Approve::Auto).await;
    let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
    let ws = f.dir.join("workspace");
    std::fs::write(ws.join("chart.png"), png).unwrap();
    std::fs::write(ws.join("brief.docx"), docx("Ship on Friday")).unwrap();
    std::fs::write(ws.join("blob.bin"), (0..2048u32).map(|i| (i * 7 % 256) as u8).collect::<Vec<_>>()).unwrap();

    let (sid, _) = f.go(&f.agent("vision = true"), "look").await;
    let outs = f.tool_outputs(&sid);
    assert!(outs[0].contains("chart.png") && outs[0].contains("attached as an image"), "{}", outs[0]);
    assert_eq!(outs[1], "Ship on Friday");
    assert!(outs[2].contains("binary file"), "{}", outs[2]);

    // The model's next request carries the image right after the tool results.
    let msgs = f.mock.reqs()[1]["messages"].as_array().unwrap().clone();
    let last = msgs.last().unwrap();
    assert_eq!(last["role"], "user");
    let url = last["content"][1]["image_url"]["url"].as_str().unwrap();
    assert!(url.starts_with("data:image/png;base64,iVBORw0KGgo"), "{url}");
}

#[tokio::test]
async fn an_image_read_does_not_start_a_new_turn_for_the_review() {
    // The images a tool returns come back as a user-role message; the review must still see the person's task and
    // the change made before the image, not an empty task and "you have not changed anything".
    let script =
        vec![call(&[("c0", "bash", json!({"command": "echo y > f"}))]), call(&[("c1", "read_file", json!({"path": "chart.png"}))]), say("done"), say("OK")];
    let f = fx(script, Approve::Auto).await;
    std::fs::write(f.dir.join("workspace/chart.png"), [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13]).unwrap();
    let (_, out) = f.go(&f.agent("vision = true\nreview = true"), "write y to f, then describe chart.png").await;
    assert_eq!(out.unwrap(), "done", "an OK review ends the run: no 'not changed anything' note");
    let reqs = f.mock.reqs();
    assert_eq!(reqs.len(), 4);
    let review = reqs[3]["messages"][1]["content"].as_str().unwrap();
    assert!(review.starts_with("# Task\nwrite y to f, then describe chart.png"), "{review}");
}

#[tokio::test]
async fn a_model_without_vision_is_told_instead_of_sent_an_image() {
    let f = fx(vec![call(&[("c1", "read_file", json!({"path": "chart.png"}))]), say("done")], Approve::Auto).await;
    std::fs::write(f.dir.join("workspace/chart.png"), [0x89, b'P', b'N', b'G']).unwrap();
    let (sid, _) = f.go(&f.agent("vision = false"), "look").await;
    assert!(f.tool_outputs(&sid)[0].contains("cannot see images"));
    assert!(f.mock.reqs()[1]["messages"].as_array().unwrap().iter().all(|m| !m["content"].is_array()));
}

#[tokio::test]
async fn read_file_transcribes_audio_with_a_model_that_hears() {
    // The second scripted reply answers the transcription call, not the agent.
    let f = fx(vec![call(&[("c1", "read_file", json!({"path": "memo.mp3"}))]), say("Two cups of strawberries."), say("done")], Approve::Auto).await;
    std::fs::write(f.dir.join("workspace/memo.mp3"), b"ID3fake-audio").unwrap();
    let (sid, _) = f.go(&f.agent("audio_model = \"mock\""), "listen").await;
    let out = &f.tool_outputs(&sid)[0];
    assert!(out.contains("Transcript of memo.mp3") && out.contains("Two cups of strawberries."), "{out}");
    let heard = &f.mock.reqs()[1]["messages"][0]["content"][1];
    assert_eq!(heard["type"], "input_audio");
    assert_eq!(heard["input_audio"]["format"], "mp3");
    assert_eq!(heard["input_audio"]["data"], "SUQzZmFrZS1hdWRpbw==");
    assert_eq!(f.app.store.stats(&sid).unwrap().model_calls, 3, "the transcription counts toward the session");

    // No model that hears: the agent is told, and nothing is sent.
    let f = fx(vec![call(&[("c1", "read_file", json!({"path": "memo.mp3"}))]), say("done")], Approve::Auto).await;
    std::fs::write(f.dir.join("workspace/memo.mp3"), b"ID3").unwrap();
    let (sid, _) = f.go(&f.agent(""), "listen").await;
    assert!(f.tool_outputs(&sid)[0].contains("no available model takes audio"));
    assert_eq!(f.mock.reqs().len(), 2);
}

#[tokio::test]
async fn a_long_page_comes_a_part_at_a_time_and_html_comes_as_served() {
    use axum::{response::Html, routing::get, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/long", listener.local_addr().unwrap());
    let body: String = (0..200).map(|i| format!("<p>row {i:03} <a href=\"/ps/{i}\">ps</a></p>")).collect();
    let html = format!("<html><head><title>List</title></head><body>{body}</body></html>");
    let served = html.clone();
    tokio::spawn(async move { axum::serve(listener, Router::new().route("/long", get(move || async move { Html(served) }))).await.unwrap() });

    // What web_fetch builds for the page, to know where the first part ends.
    let rows: String = (0..200).map(|i| format!("row {i:03} ps\n")).collect();
    let page = format!("# List\n{url}\n\n{}", rows.trim_end());
    let next = page[..1000].rfind('\n').unwrap();
    let calls = vec![
        call(&[("c1", "web_fetch", json!({"url": url}))]),
        call(&[("c2", "web_fetch", json!({"url": url, "offset": next}))]),
        call(&[("c3", "web_fetch", json!({"url": url, "format": "html"}))]),
        say("done"),
    ];
    let f = fx(calls, Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("search = \"searxng\"\npage_max = 1000"), "go").await;
    let outs = f.tool_outputs(&sid);
    let (first, note) = outs[0].split_once("\n[").unwrap();
    assert_eq!(first, &page[..next], "the first part is the top of the page, cut at a line");
    assert!(note.contains(&format!("call web_fetch again with offset={next}")), "{note}");
    let parked = format!("/tmp/.egin/out/{}/c1.txt", sid.replace(|c: char| !c.is_alphanumeric(), "_"));
    assert!(note.contains(&parked), "the full page is parked for grep: {note}");
    // The next part picks up exactly where the first stopped.
    assert!(outs[1].starts_with(&page[next..next + 40]), "{}", &outs[1][..80]);
    assert!(outs[2].contains("<a href=\"/ps/0\">ps</a>"), "html keeps the links: {}", &outs[2][..200]);
    assert_eq!(std::fs::read_to_string(&parked).unwrap(), page);
}

#[tokio::test]
async fn with_exclude_echo_a_page_that_quotes_the_task_is_withheld() {
    use axum::{response::Html, routing::get, Router};
    let task =
        "In the 1998 survey \"Rivers of the Northern Territories and Their Seasonal Flow\", which river did the authors say floods earliest each spring, and in which month?";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        // A benchmark write-up: the question word for word, then its answer.
        .route(
            "/leak",
            get(|| async { Html("<p>Q: which river did the authors say FLOODS earliest, each spring, and in which month? Ground truth: the Tanana.</p>") }),
        )
        // The survey itself: it carries the title the task quotes, and nothing else of the task.
        .route("/source", get(|| async { Html("<h1>Rivers of the Northern Territories and Their Seasonal Flow</h1><p>The Tanana floods first.</p>") }));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let fetch = |id: &'static str, path: &str| call(&[(id, "web_fetch", json!({"url": format!("{base}{path}")}))]);
    let f = fx(vec![fetch("c1", "/leak"), fetch("c2", "/source"), say("done")], Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("search = \"searxng\"\nexclude_echo = true"), task).await;
    let outs = f.tool_outputs(&sid);
    assert!(outs[0].contains("page withheld") && !outs[0].contains("Tanana"), "{}", outs[0]);
    assert!(outs[1].contains("The Tanana floods first."), "a title the task quotes does not hide its source: {}", outs[1]);

    // Off by default.
    let f = fx(vec![fetch("c1", "/leak"), say("done")], Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("search = \"searxng\""), task).await;
    assert!(f.tool_outputs(&sid)[0].contains("Tanana"));
}

/// A SearXNG instance with one search answer and one HTML page; returns its base URL.
async fn fake_searxng() -> String {
    use axum::{response::Html, routing::get, Json, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let page_url = format!("{base}/page");
    let router = Router::new()
        .route(
            "/search",
            get(move || async move {
                Json(json!({"results": [
                    {"title": "Release notes", "url": page_url, "content": "Rust 1.100 is out."},
                    {"title": "Leak", "url": "https://answers.example.com/key", "content": "the answer"},
                    {"title": "Long read", "url": "https://long.test/a", "content": "z".repeat(5000)},
                    {"title": "Last", "url": "https://last.test/b", "content": "the last hit survives"},
                ]}))
            }),
        )
        .route(
            "/page",
            get(|| async {
                Html("<html><head><title>Notes</title><script>x()</script></head><body><h1>Rust 1.100</h1><p>Shipped &amp; stable.</p></body></html>")
            }),
        );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

#[tokio::test]
async fn searxng_searches_and_pages_are_fetched_directly() {
    let base = fake_searxng().await;
    std::env::set_var("SEARXNG_URL", &base);
    let calls = vec![
        call(&[("c1", "web_search", json!({"query": "rust release"}))]),
        call(&[("c2", "web_fetch", json!({"url": format!("{base}/page")}))]),
        say("done"),
    ];
    let f = fx(calls, Approve::Auto).await;
    let (sid, _) = f.go(&f.agent("search = \"searxng\"\nexclude_domains = [\"example.com\"]\npage_max = 900"), "go").await;
    let outs = f.tool_outputs(&sid);
    assert!(outs[0].contains("Release notes") && outs[0].contains("Rust 1.100 is out."), "{}", outs[0]);
    // A long hit is trimmed to its share of the page; the hits after it are still there, whole.
    assert!(outs[0].contains("zzz […]") && outs[0].contains("the last hit survives") && outs[0].len() <= 900, "{}", outs[0]);
    assert!(!outs[0].contains("the answer"), "exclude_domains applies to every provider");
    assert!(outs[1].contains("# Notes") && outs[1].contains("Rust 1.100\nShipped & stable."), "{}", outs[1]);
    assert!(!outs[1].contains("x()"), "scripts are dropped");
}
