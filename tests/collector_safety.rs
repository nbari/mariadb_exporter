//! Structural guards for the two scrape-safety invariants introduced by the fixes for
//! `nbari/pg_exporter#34` (the scrape gate must always reopen) and `nbari/pg_exporter#35`
//! (no collector may read the operating system on a Tokio worker).
//!
//! These are source-shape assertions rather than behaviour tests on purpose. Both bugs
//! are reintroduced by an ordinary-looking refactor — moving a read one level down into a
//! helper, or evaluating a thunk eagerly — which no realistic runtime test catches
//! deterministically. The behavioural halves live next to the code they cover, in
//! `src/collectors/registry.rs` (`gate_tests`) and `src/collectors/system/process.rs`.

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

/// Source-text markers for blocking operating-system access.
///
/// Deliberately an enumeration of the shapes in use or plausibly introduced here: a
/// /proc read through a *new* dependency (or a plain-imported `Command`) matches no
/// marker until the list grows. That boundary is the price of a text scan; the
/// `KNOWN_OS_COLLECTORS` anchor keeps it honest for the collectors that read today.
const OS_READ_MARKERS: [&str; 13] = [
    "std::fs::",
    "::read(",
    "::read_dir(",
    "::read_to_string(",
    "File::open(",
    "OpenOptions::new(",
    "sysinfo",
    "sysctlbyname",
    "refresh_processes",
    "refresh_memory",
    "refresh_cpu",
    "procfs::",
    "std::process::Command",
];

/// Import roots whose items block the calling thread when invoked. Used only to notice
/// that a `use` renamed one of them, which would hide every later call from the literal
/// markers above.
const OS_IMPORT_ROOTS: [&str; 6] = [
    "std::fs",
    "std::io",
    "sysinfo",
    "libc",
    "procfs",
    "std::process",
];

/// `Collector` trait entry points, excluded from the blocking call-graph.
///
/// They are what this guard checks, never a helper it traces into: every `collect_once`
/// legitimately reaches a sampler through its offload, so seeding them would mark them
/// blocking and propagate outward from there. `collect` is the load-bearing exclusion —
/// matching is a substring test, so a blocking `collect` would make every line containing
/// `Iterator::collect()` look like a call into an OS read.
const TRAIT_ENTRY_POINTS: [&str; 3] = ["collect", "collect_once", "collect_all"];

/// `nbari/pg_exporter#35`: several collectors read the operating system with blocking,
/// synchronous APIs — `std::fs` on `/proc`, `sysctlbyname`, `sysinfo` refreshes. Running
/// any of that inline in `collect_once` occupies a Tokio worker for the whole read, which
/// stops the `sqlx` pool's futures from being polled and makes unrelated collectors fail
/// with a badly misleading `pool timed out while waiting for an open connection`.
///
/// Any `collect_once` that reaches an OS read must therefore hand the work to
/// `blocking::offload_coalesced` rather than calling its sampler directly. This covers all
/// of `src/collectors/`, not just the `system` collector where the problem was found.
///
/// # Why this is a call-graph check and not a line match
///
/// Matching OS markers only against the literal text of `collect_once` is trivially
/// defeated by the most natural refactor there is: move the read one level down into a
/// helper and call the helper. The helper still runs on the runtime worker, the marker no
/// longer appears in `collect_once`, and an `offload_coalesced` call sitting next to it
/// still satisfies a text check — so the guard passes while the issue is back.
///
/// So every function in the crate's collector tree is classified first: a function is
/// *blocking* if it contains an OS marker, or if it calls a blocking function — including
/// one defined in a different module. `collect_once` may then reference a blocking
/// function only from inside the argument list of `blocking::offload_coalesced(..)`, which
/// is the one place the work does not run on a runtime worker. Renamed imports
/// (`use std::fs::read_to_string as slurp;`) contribute their alias as an extra marker, so
/// aliasing does not hide a read either.
///
/// Scope: the whole crate. Functions are classified across every file at once, so a
/// helper defined in one module (say `util.rs`) — or outside the collector tree
/// entirely — and called from a `collect_once` is traced into: moving the read across
/// a module or directory boundary is not an escape.
#[test]
fn collectors_do_not_block_the_runtime_with_os_reads() -> Result<()> {
    /// Files known to sample the OS from `collect_once` today. Listed only so this test
    /// cannot quietly decay into a no-op if a marker above stops matching; a new
    /// collector is covered automatically and does not need to be added here.
    ///
    /// `src/collectors/tls/certificate.rs` is deliberately absent: unlike its `pg_exporter`
    /// counterpart it never opens a file, it only parses timestamps that arrived over SQL.
    const KNOWN_OS_COLLECTORS: [&str; 4] = [
        "src/collectors/exporter/process.rs",
        "src/collectors/system/cpu.rs",
        "src/collectors/system/memory.rs",
        "src/collectors/system/process.rs",
    ];

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    let mut checked: Vec<String> = Vec::new();

    // Classify the whole crate — not just src/collectors — before checking any
    // `collect_once`: a helper moved to a module *outside* the collector tree must
    // still resolve as blocking, or moving a /proc read one directory up would escape
    // the guard entirely.
    let sources = collector_sources(&root.join("src"))?;
    let markers = os_read_markers(&sources);
    let blocking_fns = blocking_functions(&sources, root, &markers);

    for (path, production) in &sources {
        let relative = path.strip_prefix(root).unwrap_or(path.as_path());

        let Some(collect_once) = production.split("fn collect_once").nth(1) else {
            continue;
        };

        // Bound the slice to the body of collect_once, which ends at the next item.
        let body = collect_once
            .split("\n    fn ")
            .next()
            .unwrap_or(collect_once);

        let masked_body = mask_rust_non_code(body);
        let body_lines: Vec<&str> = masked_body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();

        // A file may touch the OS without `collect_once` doing so: `default/version.rs`
        // reads total memory once in its constructor, which never runs on a scrape. Only
        // demand an offload from collectors whose scrape path actually reaches a read.
        let reaches_os = body_lines.iter().any(|line| {
            markers.iter().any(|marker| line.contains(marker.as_str()))
                || blocking_fns.iter().any(|(name, _)| names_word(line, name))
        });
        if !reaches_os {
            continue;
        }
        checked.push(relative.to_string_lossy().into_owned());

        if !body_lines
            .iter()
            .any(|line| line.contains("blocking::offload_coalesced("))
        {
            failures.push(format!(
                "{} runs collect_once without blocking::offload_coalesced: synchronous OS reads \
                 must be offloaded and capped at one submitted sample per collector, or aborted \
                 scrapes can pile work onto the blocking pool",
                relative.display()
            ));
        }

        // Merely *mentioning* blocking::offload_coalesced is not enough. Every OS read, and
        // every function that reaches one, must be handed to the offload rather than called
        // beside it.
        let offloaded = offload_argument_lines(&body_lines);

        for (line, is_offloaded) in body_lines.iter().zip(offloaded.iter()) {
            if *is_offloaded {
                continue;
            }

            if let Some(marker) = markers.iter().find(|marker| line.contains(marker.as_str())) {
                failures.push(format!(
                    "{} reads the OS directly inside collect_once ('{marker}' in '{line}'): move \
                     the read into a named sampler reached only through \
                     blocking::offload_coalesced, or an inline slow read starves every other \
                     collector",
                    relative.display()
                ));
            }

            if let Some((called, module)) =
                blocking_fns.iter().find(|(name, _)| names_word(line, name))
            {
                failures.push(format!(
                    "{} names the blocking function '{module}::{called}' from collect_once \
                     outside the blocking::offload_coalesced argument list ('{line}'): it \
                     reaches an OS read, so calling it here — directly or by fn value — runs \
                     that read on a Tokio worker. Pass it to offload_coalesced instead",
                    relative.display()
                ));
            }
        }
    }

    for expected in KNOWN_OS_COLLECTORS {
        if !checked.iter().any(|path| path == expected) {
            failures.push(format!(
                "{expected} is no longer recognised as sampling the OS from collect_once: either \
                 it genuinely stopped (update KNOWN_OS_COLLECTORS) or OS_READ_MARKERS stopped \
                 matching it, which would silently stop enforcing the offload for every collector"
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(failures.join("\n")))
    }
}

/// An OS sample that did not complete must **not** fail the scrape.
///
/// This is the trap the offload introduces. `blocking::offload_coalesced` is fallible — the
/// blocking task can panic or be cancelled — so the obvious `.await?` turns a host-metrics
/// hiccup into a collector `Err`. In this exporter a collector `Err` makes the registry
/// withhold **every** database-dependent family for that scrape, so a panic while reading
/// `/proc` would blank out the MariaDB metrics. Before the offload these collectors had no
/// error path at all and could not do that.
///
/// They must therefore warn and preserve, exactly as they already do for an unreadable
/// process table.
#[test]
fn an_incomplete_os_sample_does_not_fail_the_scrape() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    let mut checked = 0_usize;

    for path in rust_files_under(&root.join("src"))? {
        let source = std::fs::read_to_string(&path)?;
        let production = production_source(&source);
        let relative = path.strip_prefix(root).unwrap_or(path.as_path());

        let Some(collect_once) = production.split("fn collect_once").nth(1) else {
            continue;
        };
        let body = collect_once
            .split("\n    fn ")
            .next()
            .unwrap_or(collect_once);
        if !body.contains("offload_coalesced(") {
            continue;
        }
        checked += 1;

        // Every offload in the body, not just the first: a second sampler added later must
        // settle its failure the same way.
        let masked_body = mask_rust_non_code(body);
        for (call_at, _) in masked_body.match_indices("offload_coalesced(") {
            check_offload_call(relative, body, call_at, &mut failures);
        }

        if !masked_body.contains("if let Err(error)") && !masked_body.contains("warn!") {
            failures.push(format!(
                "{} does not report a failed OS sample: an offload error must be warned about \
                 and the previous values preserved, not silently discarded",
                relative.display()
            ));
        }
    }

    if checked == 0 {
        failures.push(
            "no collect_once uses blocking::offload_coalesced any more; this guard has become \
             a no-op"
                .to_string(),
        );
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(failures.join("\n")))
    }
}

/// Checks one `blocking::offload_coalesced(..)` call inside a `collect_once` body for
/// error propagation: `?` applied to the call's own statement, `?` applied later to a
/// binding holding its result, or a hand-rolled `return Err(binding)`.
fn check_offload_call(relative: &Path, body: &str, call_at: usize, failures: &mut Vec<String>) {
    let tail = body.get(call_at..).unwrap_or_default();

    // `?` applied to the offload's own statement — `.await?`, or a conversion
    // chain such as `.await.map_err(..)?`. The statement ends at the first `;`
    // at parenthesis depth zero, so a *later*, unrelated `.await?` in the same
    // `collect_once` is not misattributed to the offload.
    let statement = {
        let end = statement_end(tail);
        tail.get(..end).unwrap_or(tail)
    };
    let masked_statement = mask_rust_non_code(statement);
    if let Some(await_at) = masked_statement.find(".await") {
        let after_await = masked_statement
            .get(await_at + ".await".len()..)
            .unwrap_or_default()
            .trim_start();
        let propagated = after_await.starts_with('?')
            || after_await
                .trim_end()
                .trim_end_matches(';')
                .trim_end()
                .ends_with('?');
        if propagated {
            failures.push(format!(
                "{} applies `?` to blocking::offload_coalesced in collect_once: a panicked \
                 or cancelled OS sample would become a collector Err, and a collector Err \
                 makes the registry withhold every database-dependent family for that \
                 scrape. Warn and preserve instead",
                relative.display()
            ));
        }
    }

    // `?` is only the blunt form. Taking the error apart and handing it back is the
    // same bug with more steps, so follow the names the offload's result is bound to
    // and reject returning any of them — or re-applying `?` to the binding one
    // statement later (`let result = offload(..).await; ...; let _ = result?;`).
    let line_start = body
        .get(..call_at)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |at| at.saturating_add(1));
    let prefix = body.get(line_start..call_at).unwrap_or_default();

    let bindings = offload_error_bindings(prefix, tail);
    for line in mask_rust_non_code(tail)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        for binding in &bindings {
            if propagates_with_question(line, binding) {
                failures.push(format!(
                    "{} propagates the offload error from collect_once (`{binding}?`): a \
                     panicked or cancelled OS sample would become a collector Err, and a \
                     collector Err makes the registry withhold every database-dependent \
                     family for that scrape. Warn and preserve instead",
                    relative.display()
                ));
            }
        }
    }

    for binding in bindings {
        // `Err(binding)` verbatim, or any method chain on it (`.into()`,
        // `.context(..)`): the error still leaves `collect_once`.
        let masked_tail = mask_rust_non_code(tail);
        if masked_tail.contains(&format!("return Err({binding})"))
            || masked_tail.contains(&format!("return Err({binding}."))
        {
            failures.push(format!(
                "{} returns the offload error from collect_once (`return \
                 Err({binding})`): propagating it by hand blanks every \
                 database-dependent family just as `?` would. Warn and preserve instead",
                relative.display()
            ));
        }
    }
}

/// The `system.process` collector accumulates a monotonic CPU counter from per-PID deltas,
/// so the order of "sample" and "publish the new baseline" is load-bearing.
///
/// The one-slot submission guard prevents normal scrape-driven overlap. The baseline lock
/// is still a necessary defence for direct/internal calls and future refactors: if a
/// sample happened outside it, a newer pass could publish its baseline first, the older
/// pass would then count no delta and overwrite the baseline with its own lower totals,
/// and the pass after that would re-count the interval between them — inflating
/// `mariadb_system_process_group_cpu_seconds_total` above the CPU actually consumed.
///
/// The lock is a `try_lock`: an overlapping direct collection skips rather than queueing.
/// The skip path is pinned behaviourally by
/// `a_concurrent_collection_skips_instead_of_interleaving_samples` in process.rs; this
/// guard pins the shape that behaviour depends on.
///
/// This is a source-order assertion rather than a race reproduction: the interleaving
/// needs a pause between the sample and the lock, which cannot be injected from outside,
/// and a timing-based test would be flaky in both directions.
#[test]
fn process_group_locks_the_cpu_baseline_before_sampling() -> Result<()> {
    let production = production_source(&std::fs::read_to_string(process_rs())?);

    let body = production
        .split("fn sample_and_publish")
        .nth(1)
        .ok_or_else(|| anyhow!("process.rs no longer has a sample_and_publish to check"))?
        .split("\n    }")
        .next()
        .unwrap_or_default();

    let lock_at = body
        .find("self.prev_cpu.try_lock()")
        .ok_or_else(|| anyhow!("sample_and_publish must take prev_cpu with try_lock"))?;
    let sample_at = body
        .find("let observed = sample();")
        .ok_or_else(|| anyhow!("sample_and_publish no longer samples through its closure"))?;

    if lock_at > sample_at {
        return Err(anyhow!(
            "sample_and_publish samples before taking the prev_cpu lock: two overlapping \
             collections can then publish their baselines out of order and \
             mariadb_system_process_group_cpu_seconds_total over-reports"
        ));
    }

    // The platform gate must reach that implementation rather than sampling beside it.
    let gate = production
        .split("fn collect_stats_with")
        .nth(1)
        .ok_or_else(|| anyhow!("process.rs no longer has a collect_stats_with to check"))?
        .split("\n    }")
        .next()
        .unwrap_or_default();
    if !gate.contains("self.sample_and_publish(sample)") {
        return Err(anyhow!(
            "collect_stats_with no longer delegates to sample_and_publish: the production \
             sampler would bypass the baseline lock"
        ));
    }

    // And the production entry point must route every platform's sampler through the gate.
    let dispatch = production
        .split("fn collect_stats(")
        .nth(1)
        .ok_or_else(|| anyhow!("process.rs no longer has a collect_stats to check"))?
        .split("\n    }")
        .next()
        .unwrap_or_default();
    if !dispatch.contains("collect_stats_with(|| sample_processes(") {
        return Err(anyhow!(
            "collect_stats no longer routes sample_processes through collect_stats_with: the \
             production sampler would bypass the baseline lock"
        ));
    }

    let code_lines = production
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"));
    for call in code_lines
        .clone()
        .filter(|line| line.contains("sample_processes(") && !line.contains("fn sample_processes"))
    {
        if !call.starts_with("self.collect_stats_with(|| sample_processes(") {
            return Err(anyhow!(
                "unexpected process sampler call site '{call}': production sampling must happen \
                 inside collect_stats_with so prev_cpu is already locked"
            ));
        }
    }

    let sampler_uses = code_lines
        .filter(|line| line.contains("sample_processes"))
        .count();
    if sampler_uses != 4 {
        return Err(anyhow!(
            "expected exactly four sample_processes references (Linux/FreeBSD definitions and \
             their locked call sites), found {sampler_uses}"
        ));
    }

    Ok(())
}

/// `nbari/pg_exporter#35`: `/proc/<pid>/smaps_rollup` makes the kernel walk every
/// page-table entry of every mapping, costing `O(processes x resident pages)` — 13.9s of a
/// 15s scrape budget on a production primary. It must stay reachable only through the
/// explicit `--system.process-memory=pss` opt-in, never from a default code path.
#[test]
fn smaps_rollup_stays_behind_the_pss_opt_in() -> Result<()> {
    let production = production_source(&std::fs::read_to_string(process_rs())?);

    // Only real code: doc comments discuss smaps_rollup at length by design.
    let code_lines = || {
        production
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//"))
    };

    let readers: Vec<&str> = code_lines()
        .filter(|line| line.contains("/smaps_rollup") || line.contains("\"smaps_rollup\""))
        .collect();

    if readers.len() != 1 {
        return Err(anyhow!(
            "expected exactly one smaps_rollup reference in production code (the read inside \
             read_pss_bytes), found {}: {readers:?}",
            readers.len()
        ));
    }

    // read_pss_bytes must only ever be handed to the dispatcher as a lazy
    // `|| read_pss_bytes(pid)` thunk. Called eagerly — passed as a value, or read before
    // the match — every scrape would pay for the walk regardless of the configured source.
    for call in code_lines()
        .filter(|line| line.contains("read_pss_bytes(") && !line.contains("fn read_pss_bytes"))
    {
        if !call.starts_with("|| read_pss_bytes(pid)") {
            return Err(anyhow!(
                "unexpected read_pss_bytes call site '{call}': PSS must stay a lazy thunk \
                 reached only via --system.process-memory=pss, never evaluated eagerly"
            ));
        }
    }

    // The identifier itself, not just the call shape: exactly one definition and one lazy
    // thunk. A function-reference detour (`let f = read_pss_bytes; f(pid)`) would add an
    // occurrence without tripping the call-site check above.
    let identifier_uses = code_lines()
        .filter(|line| line.contains("read_pss_bytes"))
        .count();
    if identifier_uses != 2 {
        return Err(anyhow!(
            "expected exactly 2 references to read_pss_bytes in production code (its definition \
             and the lazy thunk in read_memory_bytes), found {identifier_uses}: the PSS walk must \
             keep a single, visible, lazy call site"
        ));
    }

    // The dispatch itself — that the RSS arm never calls the PSS reader — is asserted
    // behaviourally by `rss_mode_never_touches_the_smaps_rollup_reader` in process.rs,
    // which counts calls to an injected reader. This only pins the laziness it relies on.
    if !production.contains("ProcessMemorySource::Rss => statm()") {
        return Err(anyhow!(
            "process.rs no longer dispatches the default RSS source to the statm reader; PSS \
             must not become the default again"
        ));
    }

    Ok(())
}

/// The panic containment in `registry::collect_with_outcome`, `ScrapeError::TaskFailed`, and
/// `blocking::offload` is all built on `catch_unwind`, which catches *unwinding* panics only.
///
/// `panic = "abort"` in a profile turns every panic into an immediate `SIGABRT`, so all three
/// become silently inert — and only in the builds that actually ship, because the `test`
/// profile inherits `dev` and unwinds. The tests asserting containment would keep passing
/// while a single panicking collector took the whole exporter down.
///
/// The manifest is parsed as TOML rather than matched as text: `panic = 'abort'` is a TOML
/// literal string carrying the same value as `panic = "abort"`, so a text match on one
/// spelling is bypassed by the other. `.cargo/config.toml` can carry profile overrides too
/// and is checked the same way.
///
/// Environment-only overrides remain outside a source-tree guard:
/// `CARGO_PROFILE_RELEASE_PANIC=abort` and a process-level `RUSTFLAGS=-Cpanic=abort`.
/// Checked-in rustflags in `.cargo/config.toml` are parsed below, wherever they are nested.
#[test]
fn panic_containment_is_not_disabled_in_release() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let manifest_path = root.join("Cargo.toml");
    let manifest: toml::Table = std::fs::read_to_string(&manifest_path)?.parse()?;

    // The profile table must exist and contain `release`: its absence would pass the loop
    // below vacuously while the shipped binary quietly lost the documented settings.
    let profiles = manifest
        .get("profile")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| {
            anyhow!("Cargo.toml has no [profile] table; [profile.release] must exist and unwind")
        })?;
    if !profiles.contains_key("release") {
        return Err(anyhow!(
            "Cargo.toml has no [profile.release] table; the release profile must exist so the \
             absence of panic = \"abort\" is a checked property, not a default"
        ));
    }
    check_profiles_unwind(&manifest_path.display().to_string(), profiles)?;

    let config_path = root.join(".cargo").join("config.toml");
    if config_path.exists() {
        let config: toml::Table = std::fs::read_to_string(&config_path)?.parse()?;
        if let Some(profiles) = config.get("profile").and_then(toml::Value::as_table) {
            check_profiles_unwind(&config_path.display().to_string(), profiles)?;
        }
        check_rustflags_unwind(
            &config_path.display().to_string(),
            &toml::Value::Table(config),
        )?;
    }

    Ok(())
}

/// Cargo config accepts `-Cpanic=abort` in global, target-specific, and runner-specific
/// rustflags. Walk the parsed document so moving the same flag under another table cannot
/// bypass the release containment guard.
fn check_rustflags_unwind(source: &str, value: &toml::Value) -> Result<()> {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                if key == "rustflags" && rustflags_abort(child) {
                    return Err(anyhow!(
                        "{source} sets rustflags containing `-C panic=abort`; catch_unwind cannot \
                         catch an aborting panic, so this disables every panic boundary"
                    ));
                }
                check_rustflags_unwind(source, child)?;
            }
        }
        toml::Value::Array(values) => {
            for child in values {
                check_rustflags_unwind(source, child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn rustflags_abort(value: &toml::Value) -> bool {
    let mut words = Vec::new();
    match value {
        toml::Value::String(flags) => words.extend(flags.split_whitespace()),
        toml::Value::Array(flags) => {
            for flag in flags.iter().filter_map(toml::Value::as_str) {
                words.extend(flag.split_whitespace());
            }
        }
        _ => return false,
    }

    words.iter().enumerate().any(|(at, word)| {
        word.replace(' ', "") == "-Cpanic=abort"
            || (*word == "-C"
                && words
                    .get(at.saturating_add(1))
                    .is_some_and(|next| next.replace(' ', "") == "panic=abort"))
    })
}

/// Fails when any profile in a parsed `[profile]` table sets `panic` to anything but
/// `"unwind"`.
///
/// Every profile, not just `release`: `bench` and any custom profile ship the same
/// inert containment, and none of them run the tests that would notice.
fn check_profiles_unwind(
    source: &str,
    profiles: &toml::map::Map<String, toml::Value>,
) -> Result<()> {
    for (name, profile) in profiles {
        let Some(setting) = profile.get("panic") else {
            continue;
        };
        if setting.as_str() != Some("unwind") {
            return Err(anyhow!(
                "{source} sets `profile.{name}.panic = {setting}`. catch_unwind cannot catch \
                 an aborting panic, so this disables the collector panic boundary, \
                 ScrapeError::TaskFailed, and offload's panic contract — in built artifacts \
                 only, where the tests that cover them never run."
            ));
        }
    }
    Ok(())
}

/// `tokio::spawn` starts a task with an empty span stack: it does not inherit the caller's
/// `tracing` context the way an ordinary `.await` does.
///
/// Moving the scrape into a spawned task (the `nbari/pg_exporter#34` fix) therefore severed
/// every collector span from the `http.server.request` span that `make_span` builds from the
/// inbound `traceparent`. Collector spans became orphan roots, so distributed traces stopped
/// linking a `/metrics` request to the work it caused and collector logs lost `request_id`.
///
/// The scrape must be instrumented with the caller's span *before* it is handed to
/// `tokio::spawn`, not after.
#[test]
fn the_spawned_scrape_inherits_the_request_span() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let production = production_source(&std::fs::read_to_string(
        root.join("src/collectors/registry.rs"),
    )?);

    // Comments are stripped first: a comment mentioning `Span::current()` is not the
    // scrape being instrumented. The pinned shape is the attachment itself —
    // `.instrument(Span::current())` — not the bare capture, which an unused
    // `let _span = Span::current();` would satisfy without parenting anything.
    let body = strip_line_comments(
        production
            .split("async fn run_gated_scrape")
            .nth(1)
            .ok_or_else(|| anyhow!("registry.rs no longer has a run_gated_scrape to check"))?
            .split("\n}")
            .next()
            .unwrap_or_default(),
    );

    if !instruments_spawned_scrape(&body) {
        return Err(anyhow!(
            "run_gated_scrape must assign `scrape.instrument(Span::current())` to the future \
             passed to `tokio::spawn(scrape)`: an unrelated instrumented future does not parent \
             the scrape, and instrumentation inside the task is already too late"
        ));
    }

    Ok(())
}

fn instruments_spawned_scrape(body: &str) -> bool {
    let body = mask_rust_non_code(body);
    let Some(instrument_at) = body.find("let scrape = scrape.instrument(Span::current());") else {
        return false;
    };
    let Some(spawn_at) = body.find("tokio::spawn(scrape)") else {
        return false;
    };
    instrument_at < spawn_at
}

/// `nbari/pg_exporter#34`: the gate wedged because the permit was moved *into* the spawned
/// scrape task. A timeout drops the `JoinHandle`, which **detaches** the task instead of
/// cancelling it, so the permit was never released and `/metrics` answered 503 forever.
///
/// The permit must therefore be acquired by the request future and never enter the task,
/// and the handle must be wrapped so dropping it aborts rather than detaches.
#[test]
fn the_scrape_permit_is_owned_by_the_request_not_the_task() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let production = production_source(&std::fs::read_to_string(
        root.join("src/collectors/registry.rs"),
    )?);

    // Comments are stripped first: a comment that merely *mentions* the permit after the
    // spawn line is not the permit moving into the task, and must not fail the guard.
    let body = strip_line_comments(
        production
            .split("async fn run_gated_scrape")
            .nth(1)
            .ok_or_else(|| anyhow!("registry.rs no longer has a run_gated_scrape to check"))?
            .split("\n}")
            .next()
            .unwrap_or_default(),
    );

    if !body.contains("try_acquire_owned()") {
        return Err(anyhow!(
            "run_gated_scrape must take the permit with try_acquire_owned so a concurrent scrape \
             is refused rather than queued behind an already-slow server"
        ));
    }

    let permit_at = body
        .find("try_acquire_owned()")
        .ok_or_else(|| anyhow!("run_gated_scrape must bind the permit in the request future"))?;
    let spawn_at = body
        .find("tokio::spawn(")
        .ok_or_else(|| anyhow!("run_gated_scrape no longer spawns the scrape"))?;

    // The exact shape of the original bug: the permit named anywhere at or after the
    // spawn means it was moved into the task, where only the task's completion frees it.
    if body
        .get(spawn_at..)
        .is_some_and(|tail| tail.contains("permit"))
    {
        return Err(anyhow!(
            "the scrape permit is moved into the spawned task: a timeout drops the JoinHandle, \
             which detaches rather than cancels, so the permit would never be released and \
             /metrics would answer 503 forever"
        ));
    }

    if permit_at > spawn_at {
        return Err(anyhow!(
            "run_gated_scrape acquires the scrape permit after spawning: the permit must be held \
             by the request future so every exit path — timeout, panic, client disconnect — \
             releases it by ordinary Drop"
        ));
    }

    if !body.contains("AbortOnDrop(tokio::spawn(") {
        return Err(anyhow!(
            "run_gated_scrape must wrap the scrape handle in AbortOnDrop: a bare JoinHandle \
             detaches when dropped, leaving the abandoned scrape's queries holding pooled \
             connections"
        ));
    }

    // Bounded to the Drop impl itself: an `.abort()` anywhere later in the file (even in a
    // comment) must not be able to stand in for the one this invariant depends on.
    let aborts = production
        .split("impl<T> Drop for AbortOnDrop<T>")
        .nth(1)
        .and_then(|tail| tail.split("\n}").next())
        .is_some_and(|imp| strip_line_comments(imp).contains(".abort()"));
    if !aborts {
        return Err(anyhow!(
            "AbortOnDrop no longer aborts its handle on drop, which makes it a plain JoinHandle \
             and reintroduces the detached-scrape leak"
        ));
    }

    Ok(())
}

fn process_rs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/collectors/system/process.rs")
}

/// `source` with every `#[cfg(test)]` item removed.
///
/// Splitting at the *first* `#[cfg(test)]` (the obvious shortcut) truncates the file
/// there, and `system/process.rs` has a `#[cfg(test)]` helper sitting above `collect_once`
/// — so the shortcut would silently hide the very function these guards exist to check.
/// Here the attribute skips one item and scanning resumes after it, by brace balance.
fn production_source(source: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    let mut depth: i32 = 0;
    let mut opened = false;

    for line in source.lines() {
        if !skipping {
            if line.trim_start().starts_with("#[cfg(test)]") {
                skipping = true;
                depth = 0;
                opened = false;
            } else {
                kept.push(line);
            }
            continue;
        }

        depth += i32::try_from(line.matches('{').count()).unwrap_or(0);
        depth -= i32::try_from(line.matches('}').count()).unwrap_or(0);

        if line.contains('{') {
            opened = true;
        }

        // An attribute may sit above an item declared across several lines, and a
        // one-line item (`#[cfg(test)] use foo;`) never opens a brace at all.
        let item_ended = if opened {
            depth <= 0
        } else {
            line.trim_end().ends_with(';')
        };
        if item_ended {
            skipping = false;
        }
    }

    kept.join("\n")
}

/// The literal OS markers plus one per blocking item the collector tree imports under a
/// new name.
///
/// `use std::fs::read_to_string as slurp;` moves every later read behind `slurp(`, which
/// none of the literal markers match. The alias becomes a marker of its own so renaming
/// an import cannot launder a blocking read. Aliases are collected across every file:
/// where the alias was introduced says nothing about where it is used.
fn os_read_markers(sources: &[(PathBuf, String)]) -> Vec<String> {
    let mut markers: Vec<String> = OS_READ_MARKERS.iter().map(|&m| m.to_string()).collect();

    for (_, production) in sources {
        let masked = mask_rust_non_code(production);
        let mut import = String::new();
        for line in masked.lines().map(str::trim) {
            if import.is_empty() && !line.starts_with("use ") {
                continue;
            }
            if !import.is_empty() {
                import.push(' ');
            }
            import.push_str(line);
            if !line.contains(';') {
                continue;
            }

            let spec = import
                .trim_start_matches("use ")
                .trim_end_matches(';')
                .trim();
            if is_os_import(spec) {
                for binding in imported_bindings(spec) {
                    for marker in [format!("{binding}("), format!("{binding}::")] {
                        if !markers.contains(&marker) {
                            markers.push(marker);
                        }
                    }
                }
            }
            import.clear();
        }
    }

    markers
}

fn is_os_import(spec: &str) -> bool {
    let compact: String = spec
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    OS_IMPORT_ROOTS.iter().any(|root| compact.contains(root)) || compact.starts_with("std::{")
}

/// Names introduced by a `use` item. This intentionally handles the ordinary import shapes
/// used in the crate (`path::item`, aliases, and a single brace group), including groups
/// split across lines; a plain import must be as visible as its fully-qualified spelling.
fn imported_bindings(spec: &str) -> Vec<String> {
    const BLOCKING_IMPORTS: [&str; 12] = [
        "fs",
        "read",
        "read_dir",
        "read_to_string",
        "File",
        "OpenOptions",
        "process",
        "Command",
        "procfs",
        "all_processes",
        "Process",
        "sysctlbyname",
    ];

    let mut paths = Vec::new();
    if let Some((prefix, group)) = spec.split_once('{') {
        let prefix = prefix.trim().trim_end_matches("::");
        let group = group.split('}').next().unwrap_or_default();
        for item in group
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            paths.push(if item == "self" {
                prefix.to_string()
            } else {
                format!("{prefix}::{item}")
            });
        }
    } else {
        paths.push(spec.to_string());
    }

    paths
        .into_iter()
        .filter_map(|path| {
            let (original, binding) = path.rsplit_once(" as ").map_or_else(
                || {
                    let leaf = path.rsplit("::").next().unwrap_or_default();
                    (leaf, leaf)
                },
                |(original, alias)| (original.rsplit("::").next().unwrap_or_default(), alias),
            );
            let binding = binding.trim();
            (BLOCKING_IMPORTS.contains(&original.trim()) && !binding.is_empty() && binding != "*")
                .then(|| binding.to_string())
        })
        .collect()
}

/// Every function in the collector tree that performs a blocking OS read, or calls
/// something that does, paired with the module that defines it.
///
/// Attribution is by position: a line belongs to the most recent `fn` above it, which is
/// how Rust source reads and is enough to tell "this helper does the read" from "this
/// helper is merely named next to it". Propagation runs to a fixpoint over the whole tree
/// at once, so a chain of helpers is as blocking as the read at the end of it no matter
/// how many module boundaries it crosses.
///
/// Matching is by bare name, so a helper is traced from any caller without resolving
/// imports. That is deliberately conservative: two functions sharing a name are treated as
/// one, which can only ever add a demand for an offload, never drop one.
fn blocking_functions(
    sources: &[(PathBuf, String)],
    root: &Path,
    markers: &[String],
) -> Vec<(String, String)> {
    let mut spans: Vec<(String, String, Vec<String>)> = Vec::new();

    for (path, production) in sources {
        let module = module_label(path, root);
        let first_of_file = spans.len();

        for line in production.lines().map(str::trim) {
            if let Some(name) = function_name(line) {
                spans.push((name, module.clone(), Vec::new()));
            }
            // Lines above the file's first `fn` belong to no function.
            if let Some((_, _, lines)) = spans.get_mut(first_of_file..).and_then(<[_]>::last_mut) {
                lines.push(line.to_string());
            }
        }
    }

    let mut blocking: Vec<(String, String)> = Vec::new();
    let known =
        |blocking: &[(String, String)], name: &str| blocking.iter().any(|(known, _)| known == name);

    for (name, module, lines) in &spans {
        if TRAIT_ENTRY_POINTS.contains(&name.as_str()) || known(&blocking, name) {
            continue;
        }
        let reads_os = code_lines(lines).any(|line| {
            let code = mask_rust_non_code(line);
            markers.iter().any(|marker| code.contains(marker.as_str()))
        });
        if reads_os {
            blocking.push((name.clone(), module.clone()));
        }
    }

    loop {
        let mut discovered: Vec<(String, String)> = Vec::new();

        for (name, module, lines) in &spans {
            if TRAIT_ENTRY_POINTS.contains(&name.as_str())
                || known(&blocking, name)
                || known(&discovered, name)
            {
                continue;
            }
            let calls_blocking = code_lines(lines)
                .any(|line| blocking.iter().any(|(callee, _)| names_word(line, callee)));
            if calls_blocking {
                discovered.push((name.clone(), module.clone()));
            }
        }

        if discovered.is_empty() {
            return blocking;
        }
        blocking.extend(discovered);
    }
}

/// Names that hold an offload's `Result`, or the error inside it, after the call.
///
/// Two shapes reach the same place. The result may be bound first
/// (`let sample = blocking::offload_coalesced(..).await;`) and taken apart later, or
/// destructured on the spot (`if let Err(error) = blocking::offload_coalesced(..).await`).
/// `prefix` is the text before the call on its own line, which carries the first; `tail`
/// is everything after the call, which carries the second.
fn offload_error_bindings(prefix: &str, tail: &str) -> Vec<String> {
    let mut bindings = Vec::new();
    let prefix = mask_rust_non_code(prefix);
    let tail = mask_rust_non_code(tail);

    // `if let Err(error) = offload(..)` — the destructured name sits in the prefix, since
    // `tail` starts at the call itself.
    if let Some((_, bound)) = prefix.rsplit_once("if let Err(") {
        push_binding(&mut bindings, identifier(bound));
    } else if let Some((_, bound)) = prefix.rsplit_once("let ") {
        // `let result = offload(..)`. Variant names (`Err`, `Ok`, `Some`) are patterns
        // rather than bindings, and Rust bindings are snake_case, so an initial capital
        // is a reliable discriminator.
        let name = identifier(bound.trim_start().trim_start_matches("mut "));
        if !name.starts_with(|c: char| c.is_uppercase()) {
            push_binding(&mut bindings, name);
        }
    }

    for (at, _) in tail.match_indices("Err(") {
        let rest = tail
            .get(at.saturating_add("Err(".len())..)
            .unwrap_or_default();
        let name = identifier(rest);
        // Only a binding pattern or a method chain on one introduces a usable name;
        // `Err(anyhow!(..))` and friends do not. `.` covers `Err(error.into())`, which is
        // the same propagation wearing a conversion.
        let is_binding = rest
            .get(name.len()..)
            .is_some_and(|after| after.starts_with(')') || after.starts_with('.'));
        if is_binding {
            push_binding(&mut bindings, name);
        }
    }

    bindings
}

/// The leading Rust identifier of `text`.
fn identifier(text: &str) -> String {
    text.trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// Length of the statement that starts at the beginning of `text`: the first `;` at
/// parenthesis depth zero. The call under inspection may carry a closure whose body has
/// its own `;`, but those sit inside the call's parens and are skipped.
fn statement_end(text: &str) -> usize {
    let code = mask_rust_non_code(text);
    let mut depth = 0_usize;
    for (at, ch) in code.char_indices() {
        match ch {
            '(' | '[' | '{' => depth = depth.saturating_add(1),
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => return at + 1,
            _ => {}
        }
    }
    text.len()
}

/// True when `line` uses `name` as a standalone word. Both sides must be non-identifier
/// characters, so `sample` matches neither `sampler` nor `resample`.
fn names_word(line: &str, name: &str) -> bool {
    let code = mask_rust_non_code(line);
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    code.match_indices(name).any(|(at, _)| {
        let preceded = at > 0
            && code
                .as_bytes()
                .get(at.saturating_sub(1))
                .is_some_and(|prev| is_ident(*prev));
        let followed = code
            .as_bytes()
            .get(at + name.len())
            .is_some_and(|next| is_ident(*next));
        !preceded && !followed
    })
}

/// True when `line` applies `?` to the value held by `name` — directly (`name?`) or
/// through a conversion chain on the same line (`name.into()?`, `name.map_err(..)?`).
/// That is the deferred form of `.await?`: the offload error still leaves
/// `collect_once`, just one statement later.
fn propagates_with_question(line: &str, name: &str) -> bool {
    let code = mask_rust_non_code(line);
    if !names_word(&code, name) {
        return false;
    }
    for (at, _) in code.match_indices(name) {
        if code
            .as_bytes()
            .get(at + name.len())
            .is_some_and(|next| next.is_ascii_alphanumeric() || *next == b'_')
        {
            continue;
        }
        let rest = code.get(at + name.len()..).unwrap_or_default().trim_start();
        let expression = rest.split(';').next().unwrap_or(rest);
        if expression.starts_with('?') || (expression.starts_with('.') && expression.contains('?'))
        {
            return true;
        }
    }
    false
}

fn push_binding(bindings: &mut Vec<String>, name: String) {
    if !name.is_empty() && !bindings.contains(&name) {
        bindings.push(name);
    }
}

/// The lines of a function body that are not comments.
fn code_lines(lines: &[String]) -> impl Iterator<Item = &String> {
    lines.iter().filter(|line| !line.starts_with("//"))
}

/// Compatibility name for the source-shape guards: comments and literal contents are
/// masked so neither prose can impersonate a pinned code token.
fn strip_line_comments(text: &str) -> String {
    mask_rust_non_code(text)
}

/// Replaces comments and literal contents with spaces while preserving bytes and newlines.
/// Source-shape checks can then use offsets from the result against the original source,
/// without letting prose or punctuation inside a literal impersonate Rust code.
fn mask_rust_non_code(text: &str) -> String {
    let input = text.as_bytes();
    let mut output = input.to_vec();
    let mut at = 0_usize;

    while at < input.len() {
        if input.get(at..at + 2) == Some(b"//") {
            let end = input
                .get(at..)
                .and_then(|tail| tail.iter().position(|byte| *byte == b'\n'))
                .map_or(input.len(), |offset| at + offset);
            mask_bytes(&mut output, at, end);
            at = end;
            continue;
        }
        if input.get(at..at + 2) == Some(b"/*") {
            let mut cursor = at + 2;
            let mut depth = 1_usize;
            while cursor < input.len() && depth > 0 {
                if input.get(cursor..cursor + 2) == Some(b"/*") {
                    depth = depth.saturating_add(1);
                    cursor += 2;
                } else if input.get(cursor..cursor + 2) == Some(b"*/") {
                    depth = depth.saturating_sub(1);
                    cursor += 2;
                } else {
                    cursor += 1;
                }
            }
            mask_bytes(&mut output, at, cursor);
            at = cursor;
            continue;
        }

        if let Some((content_at, hashes)) = raw_string_start(input, at) {
            let mut cursor = content_at;
            let mut end = input.len();
            while cursor < input.len() {
                if input.get(cursor) == Some(&b'"')
                    && input
                        .get(cursor + 1..cursor + 1 + hashes)
                        .is_some_and(|closing| closing.iter().all(|byte| *byte == b'#'))
                {
                    end = cursor + 1 + hashes;
                    break;
                }
                cursor += 1;
            }
            mask_bytes(&mut output, at, end);
            at = end;
            continue;
        }

        let quote_at = if input.get(at) == Some(&b'"') {
            Some(at)
        } else if input
            .get(at)
            .is_some_and(|byte| *byte == b'b' || *byte == b'c')
            && input.get(at + 1) == Some(&b'"')
        {
            Some(at + 1)
        } else {
            None
        };
        if let Some(quote_at) = quote_at {
            let end = quoted_literal_end(input, quote_at, b'"').unwrap_or(input.len());
            mask_bytes(&mut output, at, end);
            at = end;
            continue;
        }

        let char_quote = if input.get(at) == Some(&b'\'') {
            Some(at)
        } else if input.get(at) == Some(&b'b') && input.get(at + 1) == Some(&b'\'') {
            Some(at + 1)
        } else {
            None
        };
        if let Some(quote_at) = char_quote
            && let Some(end) = quoted_literal_end(input, quote_at, b'\'')
        {
            mask_bytes(&mut output, at, end);
            at = end;
            continue;
        }

        at += 1;
    }

    String::from_utf8(output).unwrap_or_else(|_| " ".repeat(input.len()))
}

fn raw_string_start(input: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut cursor = at;
    if input
        .get(cursor)
        .is_some_and(|byte| *byte == b'b' || *byte == b'c')
    {
        cursor += 1;
    }
    if input.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes_at = cursor;
    while input.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    (input.get(cursor) == Some(&b'"')).then_some((cursor + 1, cursor - hashes_at))
}

fn quoted_literal_end(input: &[u8], quote_at: usize, quote: u8) -> Option<usize> {
    let mut cursor = quote_at + 1;
    while cursor < input.len() {
        let Some(byte) = input.get(cursor).copied() else {
            break;
        };
        match byte {
            b'\\' => cursor = cursor.saturating_add(2),
            byte if byte == quote => return Some(cursor + 1),
            b'\n' if quote == b'\'' => return None,
            _ => cursor += 1,
        }
    }
    None
}

fn mask_bytes(output: &mut [u8], start: usize, end: usize) {
    if let Some(range) = output.get_mut(start..end) {
        for byte in range {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
}

/// `src/collectors/system/memory.rs` -> `system::memory`, `.../system/mod.rs` -> `system`,
/// `src/osread.rs` -> `osread` (helpers may live outside the collector tree).
fn module_label(path: &Path, root: &Path) -> String {
    let src = root.join("src");
    let relative = path
        .strip_prefix(src.join("collectors"))
        .or_else(|_| path.strip_prefix(&src))
        .unwrap_or(path);
    let mut parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();

    if let Some(last) = parts.pop() {
        let stem = last.strip_suffix(".rs").unwrap_or(&last);
        if stem != "mod" {
            parts.push(stem.to_string());
        }
    }

    if parts.is_empty() {
        "collectors".to_string()
    } else {
        parts.join("::")
    }
}

/// Every `.rs` file under `dir`, paired with its source minus `#[cfg(test)]` items.
fn collector_sources(dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut sources = Vec::new();
    for path in rust_files_under(dir)? {
        let production = production_source(&std::fs::read_to_string(&path)?);
        sources.push((path, production));
    }
    Ok(sources)
}

/// The name defined by a function-definition line, if the line is one.
fn function_name(trimmed_line: &str) -> Option<String> {
    let (prefix, rest) = trimmed_line.split_once("fn ")?;

    // `fn` may only be preceded by qualifiers; anything else is prose or a type bound.
    let qualifiers_only = prefix.split_whitespace().all(|word| {
        matches!(
            word,
            "pub" | "async" | "const" | "unsafe" | "extern" | "default"
        ) || word.starts_with("pub(")
    });
    if !qualifiers_only {
        return None;
    }

    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Marks the lines of `body_lines` that sit inside a `blocking::offload_coalesced(..)`
/// argument list, tracking parenthesis depth so a multi-line call is covered exactly.
///
/// Work named there runs on the blocking pool; work named anywhere else in `collect_once`
/// runs on a Tokio worker.
fn offload_argument_lines(body_lines: &[&str]) -> Vec<bool> {
    let mut inside = Vec::with_capacity(body_lines.len());
    let mut depth: usize = 0;

    for line in body_lines {
        let code = mask_rust_non_code(line);
        let opens = code.matches('(').count();
        let closes = code.matches(')').count();

        if depth == 0 {
            if code.contains("offload_coalesced(") {
                depth = opens.saturating_sub(closes);
                inside.push(true);
            } else {
                inside.push(false);
            }
            continue;
        }

        inside.push(true);
        depth = depth.saturating_add(opens).saturating_sub(closes);
    }

    inside
}

fn rust_files_under(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }

    files.sort();
    Ok(files)
}

#[test]
fn source_scanner_ignores_literals_and_comments_without_losing_offsets() {
    let statement = r#"offload(|| { let marker = ")"; work(); }).await; later().await?;"#;
    let expected_end = statement
        .find(".await;")
        .map_or(statement.len(), |at| at + ".await;".len());
    assert_eq!(
        statement_end(statement),
        expected_end,
        "the semicolon inside the closure must remain nested despite a ')' string literal"
    );

    assert!(!names_word(r#"let note = "blocking_fn";"#, "blocking_fn"));
    assert!(names_word("let f = blocking_fn; f();", "blocking_fn"));
    assert!(propagates_with_question("let _ = result ?;", "result"));
    assert!(!propagates_with_question(
        "let _ = result; unrelated().await?;",
        "result"
    ));

    let masked = mask_rust_non_code(r#"let url = "http://host"; // permit abort()"#);
    assert!(masked.contains("let url ="));
    assert!(!masked.contains("permit"));
    assert!(!masked.contains("abort"));
}

#[test]
fn ordinary_os_imports_and_real_spawn_instrumentation_are_recognised() {
    let sources = vec![(
        PathBuf::from("probe.rs"),
        "use std::fs::{\n    read_to_string,\n};\n\
         fn probe() { read_to_string(\"/proc/version\"); }"
            .to_string(),
    )];
    let markers = os_read_markers(&sources);
    assert!(markers.iter().any(|marker| marker == "read_to_string("));

    assert!(instruments_spawned_scrape(
        "let scrape = scrape.instrument(Span::current());\n\
         let task = AbortOnDrop(tokio::spawn(scrape));"
    ));
    assert!(!instruments_spawned_scrape(
        "let unrelated = unrelated.instrument(Span::current());\n\
         let task = AbortOnDrop(tokio::spawn(scrape));"
    ));
}

#[test]
fn checked_in_panic_abort_rustflags_are_rejected() -> Result<()> {
    let config = toml::Value::Table("[build]\nrustflags = [\"-Cpanic=abort\"]\n".parse()?);
    assert!(check_rustflags_unwind("fixture", &config).is_err());

    let spaced =
        toml::Value::Table("[target.'cfg(unix)']\nrustflags = \"-C panic=abort\"\n".parse()?);
    assert!(check_rustflags_unwind("fixture", &spaced).is_err());
    Ok(())
}
