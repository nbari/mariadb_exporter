#![allow(dead_code)]

use nix::unistd::geteuid;
use std::{env, path::Path};

fn socket_exists(host: &str) -> bool {
    if let Some(path) = host.strip_prefix("unix://") {
        Path::new(path).exists()
    } else {
        true
    }
}

fn testcontainers_runtime_candidates() -> Vec<String> {
    let mut candidates = vec!["unix:///var/run/docker.sock".to_string()];
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR")
        && !runtime_dir.is_empty()
    {
        candidates.push(format!("unix://{runtime_dir}/.docker/run/docker.sock"));
    }
    if let Ok(home) = env::var("HOME")
        && !home.is_empty()
    {
        candidates.push(format!("unix://{home}/.docker/run/docker.sock"));
        candidates.push(format!("unix://{home}/.docker/desktop/docker.sock"));
    }
    candidates
}

fn find_container_runtime() -> Option<String> {
    // Honor explicit DOCKER_HOST if present and reachable.
    if let Ok(existing) = env::var("DOCKER_HOST")
        && !existing.is_empty()
        && socket_exists(&existing)
    {
        return Some(existing);
    }

    testcontainers_runtime_candidates()
        .into_iter()
        .find(|candidate| socket_exists(candidate))
}

fn detect_podman_socket() -> Option<String> {
    let uid = geteuid().as_raw();
    let candidates = [
        format!("unix:///run/user/{uid}/podman/podman.sock"),
        "unix:///run/podman/podman.sock".to_string(),
        "unix:///var/run/podman/podman.sock".to_string(),
    ];

    candidates
        .into_iter()
        .find(|candidate| socket_exists(candidate))
}

fn should_require_container_runtime() -> bool {
    let in_ci = env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true"));
    let force = env::var("MARIADB_EXPORTER_REQUIRE_TESTCONTAINERS")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE"));

    in_ci || force
}

pub fn ensure_container_runtime_for_test(test_name: &str) -> anyhow::Result<bool> {
    if find_container_runtime().is_some() {
        return Ok(true);
    }

    let mut message = format!(
        "No container runtime socket found (checked Podman + Docker), cannot run {test_name}"
    );

    if let Some(podman_socket) = detect_podman_socket() {
        message.push_str(". Podman socket detected at ");
        message.push_str(&podman_socket);
        message.push_str("; set DOCKER_HOST to this value so testcontainers can use it");
    }

    if should_require_container_runtime() {
        anyhow::bail!("{message}");
    }

    eprintln!("{message}; skipping");
    Ok(false)
}
