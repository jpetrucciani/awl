use crate::error::{AwlError, Result};

pub fn required(args: &[String], index: usize, name: &str) -> Result<String> {
    args.get(index).cloned().ok_or_else(|| AwlError::Usage {
        message: format!("missing required argument <{name}>"),
    })
}

pub fn option_value(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    args.iter().enumerate().find_map(|(idx, arg)| {
        arg.strip_prefix(&prefix)
            .map(str::to_owned)
            .or_else(|| (arg == name).then(|| args.get(idx + 1).cloned()).flatten())
    })
}

pub fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

pub fn key_values(args: &[String], name: &str) -> Result<Vec<(String, String)>> {
    let mut values = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg != name {
            continue;
        }
        let Some(value) = iter.next() else {
            return Err(AwlError::Usage {
                message: format!("{name} requires KEY=VALUE"),
            });
        };
        let Some((key, value)) = value.split_once('=') else {
            return Err(AwlError::Usage {
                message: format!("{name} requires KEY=VALUE, got {value:?}"),
            });
        };
        values.push((key.to_owned(), value.to_owned()));
    }
    Ok(values)
}

pub fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for (idx, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg.starts_with("--") {
            let takes_value = !is_boolean_flag(arg)
                && args
                    .get(idx + 1)
                    .is_some_and(|next| !next.starts_with("--"));
            skip_next = takes_value;
            continue;
        }
        out.push(arg.clone());
    }
    out
}

fn is_boolean_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--delete"
            | "--dry-run"
            | "--dryrun"
            | "--exec"
            | "--fail-fast"
            | "--force"
            | "--no-browser"
            | "--no-decrypt"
            | "--overwrite"
            | "--recursive"
            | "--strict"
            | "--tail"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn positional_keeps_values_after_boolean_dryrun_aliases() {
        let args = vec!["--dryrun".to_owned(), "i-123".to_owned()];
        assert_eq!(
            crate::services::common::positional(&args),
            vec!["i-123".to_owned()]
        );

        let args = vec!["--dry-run".to_owned(), "i-456".to_owned()];
        assert_eq!(
            crate::services::common::positional(&args),
            vec!["i-456".to_owned()]
        );
    }

    #[test]
    fn option_value_accepts_equals_and_space_forms() {
        let args = vec![
            "--limit=10".to_owned(),
            "--prefix".to_owned(),
            "/app".to_owned(),
        ];
        assert_eq!(
            crate::services::common::option_value(&args, "--limit"),
            Some("10".to_owned())
        );
        assert_eq!(
            crate::services::common::option_value(&args, "--prefix"),
            Some("/app".to_owned())
        );
    }
}
