use coshell_shell::{assess, assess_with_trusted_commands, redact, RiskLevel, RULE_VERSION};

#[test]
fn dangerous_commands_and_compounds_are_identified() {
    for command in [
        "rm -rf /tmp/demo",
        "sudo rm -r /var/tmp/a",
        "sudo -u root rm -rf /tmp/demo",
        "sudo --user root rm -rf /tmp/demo",
        "sudo --user=root rm -rf /tmp/demo",
        "eval 'rm -rf /tmp/demo'",
        "eval rm -rf /tmp/demo",
        "bash -lc 'rm -rf /tmp/demo'",
        "mkfs.ext4 /dev/sda1",
        "dd if=/dev/zero of=/dev/sda",
        "fdisk /dev/sda",
        "shutdown -h now",
        "systemctl reboot",
        "chmod -R 777 /srv",
        "chown --recursive root /srv",
        "echo ok && rm -rf /tmp/demo",
        "echo ok | sudo rm -r /tmp/demo",
        "echo $(rm -rf /tmp/demo)",
        "echo `rm -rf /tmp/demo`",
        "echo ok\nrm -r /tmp/demo",
        "sh -c 'rm -rf /tmp/demo'",
        "echo hi > /dev/sda",
        "rm -rf /tmp/foo*",
        "sudo --preserve-env=PATH rm -rf /tmp/demo",
        "(rm -rf /tmp/demo)",
        "env rm -rf /tmp/demo",
        "/usr/bin/sudo rm -rf /tmp/demo",
    ] {
        let result = assess(command);
        assert_eq!(result.level, RiskLevel::Dangerous, "{command}: {result:?}");
        assert!(!result.categories.is_empty(), "{command}");
        assert_eq!(result.rule_version, RULE_VERSION);
        assert!(!result.display_spans.is_empty(), "{command}");
    }
}

#[test]
fn only_verifiably_simple_commands_are_safe() {
    assert_eq!(
        assess_with_trusted_commands("/tmp/ls", &["ls"]).level,
        RiskLevel::Unknown
    );
    assert_eq!(
        assess_with_trusted_commands("/tmp/ls", &["/tmp/ls"]).level,
        RiskLevel::Safe
    );
    for command in ["pwd", "ls -la /tmp", "echo 'hello'", "cat /etc/os-release"] {
        assert_eq!(assess(command).level, RiskLevel::Unknown, "{command}");
        assert_eq!(
            assess_with_trusted_commands(command, &["pwd", "ls", "echo", "cat"]).level,
            RiskLevel::Safe,
            "{command}"
        );
    }
    for command in [
        "x=rm; \"$x\" -rf /tmp/a",
        "eval \"$CMD\"",
        "sh -c \"$CMD\"",
        "foo --bar",
        "ls $DIR",
        "echo `whoami`",
        "echo 'unterminated",
        "echo hello > output",
        "ls *",
        "command -v rm",
        "echo a\\\nb",
        "echo $(unknown)",
        "",
        "# comment",
        "echo hi;",
    ] {
        assert_eq!(assess(command).level, RiskLevel::Unknown, "{command}");
    }
}

#[test]
fn wrapped_danger_spans_cover_original_unicode_text() {
    for command in [
        "echo 好 && sudo --user root rm -rf /tmp/目录",
        "env rm -rf /tmp/目录",
        "(rm -rf /tmp/目录)",
        "command rm -rf /tmp/目录",
    ] {
        let result = assess(command);
        assert_eq!(result.level, RiskLevel::Dangerous, "{command}: {result:?}");
        let span = result
            .display_spans
            .iter()
            .find(|span| {
                command
                    .get(span.start..span.end)
                    .is_some_and(|text| text.contains("rm -rf /tmp/目录"))
            })
            .expect("danger span must cover original command");
        assert!(command.is_char_boundary(span.start) && command.is_char_boundary(span.end));
    }
}

#[test]
fn compound_unknown_is_not_downgraded_by_trusted_commands() {
    for command in [
        "echo ok && $CMD",
        "echo ok | sh -c \"$CMD\"",
        "echo ok; unknown",
        "command -- $CMD",
    ] {
        assert_eq!(
            assess_with_trusted_commands(command, &["echo"]).level,
            RiskLevel::Unknown,
            "{command}"
        );
    }
}

#[test]
fn read_redirection_and_operands_after_option_terminator_are_not_dangerous() {
    for command in [
        "cat < /dev/null",
        "cat </dev/sda",
        "rm -- -rf",
        "chmod -- -R",
    ] {
        let result = assess(command);
        assert_ne!(result.level, RiskLevel::Dangerous, "{command}: {result:?}");
    }
    assert_eq!(assess("echo data > /dev/sda").level, RiskLevel::Dangerous);
    assert_eq!(assess("rm -r -- -rf").level, RiskLevel::Dangerous);
}

#[test]
fn history_expansion_and_command_wrappers_are_conservative() {
    for command in ["echo !prefix", "echo \"!prefix\""] {
        assert_eq!(
            assess_with_trusted_commands(command, &["echo"]).level,
            RiskLevel::Unknown,
            "{command}"
        );
    }
    assert_eq!(
        assess_with_trusted_commands("echo '!prefix'", &["echo"]).level,
        RiskLevel::Safe
    );
    for command in [
        "time rm -rf /tmp/demo",
        "xargs rm -rf",
        "find . -exec rm -rf {} \\;",
    ] {
        assert_eq!(assess(command).level, RiskLevel::Dangerous, "{command}");
    }
}

#[test]
fn trusted_executable_does_not_make_destructive_arguments_safe() {
    for command in [
        "find /tmp/demo -delete",
        "rm -d /",
        "rm -- /./",
        "rm -d /tmp/..",
    ] {
        assert_eq!(
            assess_with_trusted_commands(command, &["find", "rm"]).level,
            RiskLevel::Dangerous,
            "{command}"
        );
    }
    for command in [
        "awk 'BEGIN {system(\"rm -rf /tmp/demo\")}'",
        "rm -f /etc/passwd",
        "find /tmp/demo -execdir sh -c evil \\;",
    ] {
        assert_ne!(
            assess_with_trusted_commands(command, &["awk", "rm", "find"]).level,
            RiskLevel::Safe,
            "{command}"
        );
    }
}

#[test]
fn redaction_never_returns_secrets_in_text_spans_or_debug() {
    for (command, secret) in [
        ("curl --token abc123 https://example.com", "abc123"),
        ("tool --password=abc123 --name ok", "abc123"),
        ("PASSWORD=abc123 command", "abc123"),
        ("export API_TOKEN='abc123'", "abc123"),
        ("mysql -pabc123", "abc123"),
        ("curl https://user:abc123@example.com", "abc123"),
        ("tool --access-key abc123", "abc123"),
        ("curl -H 'X-Api-Key: abc123' https://example.com", "abc123"),
        ("tool -P abc123", "abc123"),
        ("ls --password abc123 /tmp", "abc123"),
        ("ls /tmp/abc123", "abc123"),
        ("cat /tmp/abc123", "abc123"),
        ("cat /tmp/huntertwo", "huntertwo"),
        ("cat 12345678", "12345678"),
        ("systemctl status mycredential", "mycredential"),
        ("systemctl status token-abc123", "abc123"),
        ("rm -rf /tmp/secret-abc123", "abc123"),
        ("custom-command abc123", "abc123"),
        ("echo abc123", "abc123"),
        (
            "-----BEGIN PRIVATE KEY-----\nabc123\n-----END PRIVATE KEY-----",
            "abc123",
        ),
    ] {
        let result = redact(command);
        assert!(!result.text.contains(secret), "{}", result.text);
        assert!(!format!("{result:?}").contains(secret));
        assert!(!result.spans.is_empty(), "{command}");
        assert_eq!(result.digest.len(), 64);
        assert_eq!(result.digest, redact(command).digest);
    }
}

#[test]
fn redaction_preserves_nonsecret_commands_and_fails_closed_on_ambiguity() {
    assert_eq!(redact("pwd").text, "pwd");
    assert_eq!(redact("ls -la /tmp").text, "ls -la /tmp");
    assert!(!redact("ls abc123").text.contains("abc123"));
    let result = redact("tool --password 'unfinished-secret");
    assert!(!result.text.contains("unfinished-secret"));
    assert!(!result.spans.is_empty());
}

#[test]
fn recognized_secret_values_have_precise_spans_and_safe_context() {
    for (input, expected, secret) in [
        ("curl --user abc123", "curl --user [REDACTED]", "abc123"),
        (
            "curl --proxy-user '密钥'",
            "curl --proxy-user [REDACTED]",
            "'密钥'",
        ),
    ] {
        let result = redact(input);
        assert_eq!(result.text, expected);
        assert_eq!(result.spans.len(), 1);
        assert_eq!(&input[result.spans[0].start..result.spans[0].end], secret);
        assert_eq!(result.digest, redact(input).digest);
        assert!(!format!("{result:?}").contains(secret));
    }
    assert_eq!(redact("curl --token abc123 unknown").text, "[REDACTED]");
    assert_eq!(redact("curl --token $SECRET").text, "[REDACTED]");
}
