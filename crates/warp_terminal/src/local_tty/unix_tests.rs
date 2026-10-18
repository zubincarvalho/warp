use super::*;

#[test]
fn replacement_drops_standby_exports_and_keeps_explicit_run_environment() {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().canonicalize().unwrap();
    let command = |script: &str, environment| {
        build_host_shell_command(
            DirectShellStarter::new_for_test(
                ShellType::Bash,
                "/bin/sh".into(),
                vec![
                    "-c".into(),
                    format!("cd -- \"$WARP_INITIAL_WORKING_DIR\" || exit 1; {script}").into(),
                ],
            ),
            None,
            environment,
            Some(directory.clone()),
            false,
            false,
            false,
            false,
            false,
        )
    };
    assert!(command(
        "export WARP_STANDBY_EXPORT_LOSS_TEST_VALUE=standby; test \"$WARP_STANDBY_EXPORT_LOSS_TEST_VALUE\" = standby",
        HashMap::new(),
    ).status().unwrap().success());
    let environment = HashMap::from([
        ("WARP_API_KEY".into(), "assigned-key".into()),
        ("WARP_METADATA_TICKET_ID".into(), "ENG-42".into()),
        ("EXPECTED_CWD".into(), directory.as_os_str().to_owned()),
    ]);
    assert!(command(
        "test -z \"${WARP_STANDBY_EXPORT_LOSS_TEST_VALUE+x}\" && test \"$WARP_API_KEY\" = assigned-key && test \"$WARP_METADATA_TICKET_ID\" = ENG-42 && test \"$(pwd -P)\" = \"$EXPECTED_CWD\"",
        environment,
    ).status().unwrap().success());
}

fn shell_starter(shell_type: ShellType, shell_path: &str) -> DirectShellStarter {
    DirectShellStarter::new_for_test(shell_type, PathBuf::from(shell_path), Vec::new())
}

#[test]
fn host_replacement_command_uses_explicit_environment_and_directory() {
    let directory = tempfile::tempdir().unwrap();
    let command = build_host_shell_command(
        shell_starter(ShellType::Bash, "/bin/bash"),
        None,
        HashMap::from([
            ("WARP_API_KEY".into(), "run-key".into()),
            ("CUSTOM_SETUP_VALUE".into(), "run-value".into()),
        ]),
        Some(directory.path().to_owned()),
        false,
        false,
        false,
        false,
        true,
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_WORKING_DIR"),
        Some(Some(directory.path().to_string_lossy().into_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_API_KEY"),
        Some(Some("run-key".into()))
    );
    assert_eq!(
        env_value(&command, "CUSTOM_SETUP_VALUE"),
        Some(Some("run-value".into()))
    );
}

fn env_value(command: &Command, key: &str) -> Option<Option<String>> {
    command
        .get_envs()
        .find(|(env_key, _)| *env_key == std::ffi::OsStr::new(key))
        .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
}

#[test]
fn host_bash_command_sets_history_size_sentinels() {
    let command = build_host_shell_command(
        shell_starter(ShellType::Bash, "/bin/bash"),
        None,
        HashMap::new(),
        None,
        false,
        false,
        false,
        false,
        true,
    );

    assert_eq!(
        env_value(&command, "HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
}

#[test]
fn host_non_bash_command_does_not_set_history_size_sentinels() {
    let command = build_host_shell_command(
        shell_starter(ShellType::Zsh, "/bin/zsh"),
        None,
        HashMap::new(),
        None,
        false,
        false,
        false,
        false,
        true,
    );

    assert_eq!(env_value(&command, "HISTFILESIZE"), None);
    assert_eq!(env_value(&command, "HISTSIZE"), None);
    assert_eq!(env_value(&command, "WARP_INITIAL_HISTFILESIZE"), None);
    assert_eq!(env_value(&command, "WARP_INITIAL_HISTSIZE"), None);
}

#[test]
fn docker_sandbox_command_sets_history_size_sentinels() {
    let docker_starter =
        DockerSandboxShellStarter::new(shell_starter(ShellType::Bash, "sbx"), None);
    let command = build_docker_sandbox_command(
        &docker_starter,
        None,
        HashMap::new(),
        false,
        false,
        false,
        false,
        true,
    );

    assert_eq!(
        env_value(&command, "HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
}
