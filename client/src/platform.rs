//! The handful of things a client asks of the machine it is running on.
//!
//! Everything here has two answers, one per target, and they are in one file so that the *reasons*
//! sit next to each other rather than being rediscovered at each call site. web.md §5 is the list
//! this was written from.
//!
//! The rule the two halves are chosen by: a browser is not a worse computer, it is a different one.
//! Nothing below pretends a filesystem is there and quietly does nothing — each answer is the thing
//! that plays the same *role* on that platform. An environment variable is how you say something to
//! one process on a desktop; a query parameter is how you say it to one tab.

/// What was said to this process, or this tab, about one setting.
///
/// Named for the environment variable, which is the older of the two and the one the documentation
/// and `deploy.sh` already print. In a browser the same setting is a query parameter with the
/// prefix stripped and the rest lower-cased, so `NOOB_TUBE_BOT` is `?bot=1` — there is nothing to
/// look up, because a URL is the only thing a player can be handed that carries settings with it.
pub fn setting(name: &str) -> Option<String> {
    #[cfg(not(target_family = "wasm"))]
    {
        std::env::var(name).ok()
    }
    #[cfg(target_family = "wasm")]
    {
        let key = name.strip_prefix("NOOB_TUBE_").unwrap_or(name).to_lowercase();
        query().and_then(|params| params.get(&key))
    }
}

/// Whether a setting is on: present, and not the one word that means off.
///
/// `NOOB_TUBE_BOT=0` is off rather than "a bot, called 0", which several of these already relied on
/// separately. In a URL, `?bot` with no value at all reads as on — a parameter somebody bothered to
/// type is one they meant.
pub fn switched_on(name: &str) -> bool {
    match setting(name) {
        Some(value) => value != "0",
        None => false,
    }
}

/// A number no other client running right now will have picked.
///
/// It is the netcode client id, and two clients that share one are one client as far as the server
/// is concerned — so this is about distinctness, not about time. The clock is only the cheapest
/// source of it that needs no dependency.
///
/// **`SystemTime::now()` panics on `wasm32-unknown-unknown`** rather than returning an error, which
/// is the only entry in web.md §5 that would have taken the client down on its first frame instead
/// of quietly doing nothing. In a browser the clock is `Date.now()`, in milliseconds rather than
/// nanoseconds — coarse enough that two tabs opened together could land on the same one, so the
/// low bits come from `Math.random()` instead.
pub fn unique_id() -> u64 {
    #[cfg(not(target_family = "wasm"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos() as u64)
            .unwrap_or(0)
    }
    #[cfg(target_family = "wasm")]
    {
        let millis = js_sys::Date::now() as u64;
        let noise = (js_sys::Math::random() * 1.0e6) as u64;
        millis.wrapping_mul(1_000_000).wrapping_add(noise)
    }
}

/// Whether an asset is there to be loaded, answered now rather than in a few frames.
///
/// Three places choose between two things by this — which character kit to use, whether a clip
/// exists to be played, and whether the vehicle has a model or a box — and all three need the
/// answer before they build anything, which is why none of them asks the asset server. See
/// `build.rs`, which is where the browser's copy of the answer comes from and why it exists.
///
/// The path is relative to the asset root, the way the asset server names things.
pub fn shipped(path: &str) -> bool {
    #[cfg(not(target_family = "wasm"))]
    {
        std::path::Path::new(crate::ASSETS).join(path).exists()
    }
    #[cfg(target_family = "wasm")]
    {
        SHIPPED.lines().any(|shipped| shipped == path)
    }
}

/// What was under `assets/` when this was built, one path per line.
#[cfg(target_family = "wasm")]
const SHIPPED: &str = include_str!(concat!(env!("OUT_DIR"), "/shipped_assets.txt"));

/// One line, from before there is anywhere for a log line to go.
///
/// Everything `configure` says happens before `App::new`, so `LogPlugin` has not installed a
/// tracing subscriber and `info!` would go nowhere at all. On a desktop the answer is `println!`.
/// In a browser it is the console, and for the same reason: `println!` there writes to a stdout
/// that nothing is reading.
pub fn say(line: &str) {
    #[cfg(not(target_family = "wasm"))]
    {
        println!("{line}");
    }
    #[cfg(target_family = "wasm")]
    {
        web_sys::console::log_1(&line.into());
    }
}

/// The machine to reach the server on when nobody has said which.
///
/// Loopback on a desktop, because that is where a developer's server is. In a browser it is the
/// host that served the page, which is the same answer arrived at from the other side: the bundle,
/// the assets and the config all came from that origin, and the game is served by whoever served
/// them.
pub fn default_host() -> String {
    #[cfg(not(target_family = "wasm"))]
    {
        std::net::Ipv4Addr::LOCALHOST.to_string()
    }
    #[cfg(target_family = "wasm")]
    {
        web_sys::window()
            .and_then(|window| window.location().hostname().ok())
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| std::net::Ipv4Addr::LOCALHOST.to_string())
    }
}

/// What the page was told about the server before the wasm module started.
///
/// The whole shape of the bootstrap is here, and web.md §2 has the reasoning: everything the two
/// ends must agree on has to be settled *before* `App::new`, and in a browser nothing may block —
/// `main` has to return to the event loop, and there is no synchronous fetch and no raw TCP. So the
/// fetch happens in `index.html`, before the module is instantiated, and lands here.
///
/// A string rather than an object, deliberately. The JavaScript side does one `fetch` and one
/// `.text()`; this side hands the result to the same `serde_json` that the native client's socket
/// answer goes through, so there is one parser and one set of field names rather than two.
#[cfg(target_family = "wasm")]
pub fn preloaded_config() -> Option<noob_tube_shared::metadata::ServerInfo> {
    let window = web_sys::window()?;
    let said = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str(CONFIG_GLOBAL))
        .ok()?
        .as_string()?;
    match serde_json::from_str(&said) {
        Ok(info) => Some(info),
        Err(trouble) => {
            // Not a panic: a client with no answer keeps its own settings, exactly as one that
            // could not reach the endpoint does. But this is the case where somebody *did* answer
            // and the answer was not what this build reads, which is worth a line of its own.
            web_sys::console::error_1(&format!("{CONFIG_GLOBAL} is not readable: {trouble}").into());
            None
        }
    }
}

/// Where `index.html` leaves it. Changing this means changing the page too.
#[cfg(target_family = "wasm")]
const CONFIG_GLOBAL: &str = "NOOB_TUBE_CONFIG";

/// The tab's query parameters, or `None` when there is no document to ask.
#[cfg(target_family = "wasm")]
fn query() -> Option<web_sys::UrlSearchParams> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search).ok()
}

/// This browser's local storage, or `None` when it is switched off.
///
/// It is switched off more often than it looks: a private window, a browser set to block site data,
/// and a thumbnail capture all answer with an error rather than an empty store. Every caller has to
/// work without it, which is why this is an `Option` and not a panic.
#[cfg(target_family = "wasm")]
pub fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
