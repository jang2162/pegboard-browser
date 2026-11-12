//! `Page.createIsolatedWorld` is on the proxy's allow-list (agent-browser's `a11y` uses it) and
//! the proxy passes `grantUniveralAccess` (sic, the CDP spelling) through unchanged.
//!
//! Can the caller of the `pegboard` CLI run its own script in such a world? No allowed command
//! does: `eval`, `wait --fn` and the other script-taking forms use `Runtime.evaluate` in the
//! page's main world (checked from the CLI side by `eval_runs_in_the_main_world_and_a11y_works`
//! in the CLI isolation suite). `a11y` takes caller-supplied `--tags` and `-s` values and runs a
//! script parameterised by them; how those values are embedded was not verified, and what an
//! escape could reach is bounded by what this test shows. This test attacks the primitive at the
//! proxy level, as a client
//! that speaks raw CDP through its session endpoint would: it asks for a universal-access
//! world and tries to read local files and the human profile from it.

mod common;

use serde_json::{json, Value};

/// Evaluates in an execution context and returns the value (or the exception text).
async fn eval_in(
    client: &pegboard_core::cdp::CdpConn,
    session: &str,
    context: Option<i64>,
    expression: &str,
) -> String {
    let mut params =
        json!({ "expression": expression, "awaitPromise": true, "returnByValue": true });
    if let Some(id) = context {
        params["contextId"] = json!(id);
    }
    let r = client
        .send("Runtime.evaluate", params, Some(session))
        .await
        .unwrap();
    if let Some(ex) = r.get("exceptionDetails") {
        return format!("EXCEPTION {ex}");
    }
    match &r["result"]["value"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[tokio::test]
#[ignore]
async fn universal_access_isolated_world_cannot_escape() {
    let s = common::session_fixture().await;
    // The human really holds the data that must not show up: asserted through the
    // browser-level connection, not the session.
    let human_session = s.browser.conn.attach(&s.human_target).await.unwrap();
    let human_cookie = s
        .browser
        .conn
        .evaluate(&human_session, "document.cookie")
        .await
        .unwrap();
    assert!(
        human_cookie.as_str().unwrap_or("").contains("spike=1"),
        "the human tab has no cookie: {human_cookie}"
    );
    let client = common::client(&s.endpoint).await;
    let session = client.attach(&s.first_target).await.unwrap();
    client
        .send("Page.enable", json!({}), Some(&session))
        .await
        .unwrap();

    // A page on `localhost` (the fixture also answers there): the fixture at `127.0.0.1` is
    // another origin, which a page cannot fetch without universal access.
    let port = s.fixture.origin.rsplit(':').next().unwrap();
    client
        .navigate(&session, &format!("http://localhost:{port}/counter"))
        .await
        .unwrap();
    let tree = client
        .send("Page.getFrameTree", json!({}), Some(&session))
        .await
        .unwrap();
    let frame_id = tree["frameTree"]["frame"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let world = client
        .send(
            "Page.createIsolatedWorld",
            json!({ "frameId": frame_id, "worldName": "attack", "grantUniveralAccess": true }),
            Some(&session),
        )
        .await
        .unwrap();
    let ctx = world["executionContextId"].as_i64().unwrap();

    // Cross-origin reads from the world and from the main world, for the record. Measured on
    // Chrome 157: the world is blocked like the main world, so the flag grants no network
    // access there. Printed (not asserted) so that a change in a later Chrome shows up in
    // the output; the escape attempts below are asserted whatever this prints.
    let fixture_url = format!("{}/counter", s.fixture.origin);
    let fetch_text = format!("fetch('{fixture_url}').then(r => r.text()).then(t => t.length > 0 ? 'READ' : 'EMPTY').catch(e => 'BLOCKED ' + e)");
    let xhr_text = format!(
        "new Promise(res => {{ const x = new XMLHttpRequest(); x.onload = () => res('READ ' + x.status); x.onerror = () => res('BLOCKED'); x.open('GET', '{fixture_url}'); x.send(); }})"
    );
    for (name, expr) in [("fetch", &fetch_text), ("xhr", &xhr_text)] {
        let in_world = eval_in(&client, &session, Some(ctx), expr).await;
        let in_main = eval_in(&client, &session, None, expr).await;
        println!("cross-origin {name}: world = {in_world}, main world = {in_main}");
    }

    // The escape attempts. Each must fail (a thrown error or a refused scheme).
    for url in [
        "file:///etc/hosts",
        "file:///",
        "chrome://version/",
        "chrome://settings/",
        "chrome-extension://invalid/manifest.json",
        "filesystem:file:///temporary/x",
    ] {
        let fetch = format!("fetch('{url}').then(r => r.text()).then(t => 'READ ' + t.slice(0, 40)).catch(e => 'BLOCKED ' + e)");
        let got = eval_in(&client, &session, Some(ctx), &fetch).await;
        assert!(
            got.starts_with("BLOCKED"),
            "fetch {url} from the world: {got}"
        );
        let xhr = format!(
            "new Promise(res => {{ const x = new XMLHttpRequest(); x.onload = () => res('READ ' + x.status + ' ' + x.responseText.slice(0, 40)); x.onerror = () => res('BLOCKED'); try {{ x.open('GET', '{url}'); x.send(); }} catch (e) {{ res('BLOCKED ' + e); }} }})"
        );
        let got = eval_in(&client, &session, Some(ctx), &xhr).await;
        assert!(
            got == "BLOCKED" || got.starts_with("BLOCKED ") || got.starts_with("EXCEPTION"),
            "XHR {url} from the world: {got}"
        );
    }

    // Control: the script really runs in the isolated world, not the main world. A global set
    // by the main world is invisible in the world.
    eval_in(&client, &session, None, "window.mainOnly = 1").await;
    assert_eq!(
        eval_in(&client, &session, None, "typeof window.mainOnly").await,
        "number"
    );
    assert_eq!(
        eval_in(&client, &session, Some(ctx), "typeof window.mainOnly").await,
        "undefined",
        "the evaluation did not run in the isolated world"
    );

    // Regression watch (currently NULL in both worlds because the frame is out-of-process, so
    // it distinguishes nothing today): the page (localhost) embeds the fixture at 127.0.0.1
    // (`/cookie/get`, whose script prints the cookie and localStorage it sees) and the
    // universal-access world tries to read the other origin's document. Whatever it can read
    // must be the agent profile's data, never the human's (the human tab holds cookie
    // `spike=1` and localStorage `spike` on 127.0.0.1).
    let frame_url = format!("{}/cookie/get", s.fixture.origin);
    let load = format!(
        "new Promise(res => {{ const f = document.createElement('iframe'); f.id = 'xo'; f.onload = () => res('loaded'); f.src = '{frame_url}'; document.body.appendChild(f); }})"
    );
    assert_eq!(eval_in(&client, &session, None, &load).await, "loaded");
    let read = "(() => { try { const d = document.getElementById('xo').contentDocument; return d ? 'READ ' + d.body.innerText : 'NULL'; } catch (e) { return 'BLOCKED ' + e; } })()";
    let from_world = eval_in(&client, &session, Some(ctx), read).await;
    let from_main = eval_in(&client, &session, None, read).await;
    println!("cross-origin iframe read: world = {from_world:?}, main world = {from_main:?}");
    assert!(
        !from_world.contains("spike"),
        "the world read human-profile data: {from_world}"
    );
    if let Some(text) = from_world.strip_prefix("READ ") {
        assert_eq!(
            text.trim(),
            r#"{"cookie":"","ls":null}"#,
            "what the world read is not the agent profile's data"
        );
    }

    // Same-origin storage check, the assertion that fails if the session were in the human
    // context: on a 127.0.0.1 page of the session the universal-access world reads the
    // cookie and localStorage; the human tab holds `spike=1` on that very origin.
    client
        .navigate(&session, &format!("{}/cookie/get", s.fixture.origin))
        .await
        .unwrap();
    let tree = client
        .send("Page.getFrameTree", json!({}), Some(&session))
        .await
        .unwrap();
    let frame_id = tree["frameTree"]["frame"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let world = client
        .send(
            "Page.createIsolatedWorld",
            json!({ "frameId": frame_id, "worldName": "attack2", "grantUniveralAccess": true }),
            Some(&session),
        )
        .await
        .unwrap();
    let ctx2 = world["executionContextId"].as_i64().unwrap();
    let state = eval_in(
        &client,
        &session,
        Some(ctx2),
        "JSON.stringify({ cookie: document.cookie, ls: localStorage.getItem('spike') })",
    )
    .await;
    assert!(
        !state.contains("spike=1"),
        "the world sees the human cookie: {state}"
    );
    assert_eq!(
        state, r#"{"cookie":"","ls":null}"#,
        "the world sees the human profile's data"
    );

    // The human tab is untouched and not among the targets the session can see.
    let listed = client
        .send("Target.getTargets", json!({}), None)
        .await
        .unwrap();
    assert!(
        listed["targetInfos"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["targetId"] != s.human_target.as_str()),
        "the human tab is listed"
    );
    s.shutdown().await;
}
