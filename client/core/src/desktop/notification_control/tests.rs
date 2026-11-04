use super::*;
const ID: &str = "12345678-1234-4234-8234-123456789abc";
fn scope() -> Scope {
    Scope {
        configuration: "a".repeat(64),
        service_epoch: ID.into(),
    }
}
fn configure() -> Command {
    Command::Configure {
        request_id: ID.into(),
        journal: None,
        revision: 0,
        preferences: Preferences {
            optional_questions: true,
        },
    }
}
#[test]
fn closed_commands_cannot_claim_focus_mode_or_writer_permission() {
    for extra in [
        "focused",
        "ownership_mode",
        "enabled",
        "automatic_approvals",
    ] {
        let mut value = serde_json::to_value(Command::Focus {
            request_id: ID.into(),
        })
        .unwrap();
        value[extra] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<Command>(value).is_err(), "{extra}");
    }
    let mut value = serde_json::to_value(configure()).unwrap();
    value["preferences"]["enabled"] = true.into();
    assert!(serde_json::from_value::<Command>(value).is_err());
}
#[test]
fn initial_configuration_and_revision_limits_are_exact() {
    assert!(configure().valid());
    let mut command = configure();
    if let Command::Configure { revision, .. } = &mut command {
        *revision = 1;
    }
    assert!(!command.valid());
    if let Command::Configure {
        journal, revision, ..
    } = &mut command
    {
        *journal = Some(ID.into());
        *revision = MAX_REVISION - 1;
    }
    assert!(command.valid());
    if let Command::Configure { revision, .. } = &mut command {
        *revision = MAX_REVISION;
    }
    assert!(!command.valid());
}
#[test]
fn receipt_binds_original_value_configuration_and_identity() {
    let original = configure();
    let scope = scope();
    let receipt = Receipt {
        abi: ABI.into(),
        request_id: ID.into(),
        scope: scope.clone(),
        fingerprint: original.fingerprint(&scope.configuration).unwrap(),
        journal: Some(ID.into()),
        revision: 1,
        preferences: Preferences {
            optional_questions: true,
        },
        durable: true,
        outcome: Outcome::Saved,
    };
    assert!(receipt.matches(&original, &scope.configuration));
    let mut wrong = receipt.clone();
    wrong.preferences.optional_questions = false;
    assert!(!wrong.matches(&original, &scope.configuration));
    let mut wrong = receipt.clone();
    wrong.durable = false;
    assert!(!wrong.matches(&original, &scope.configuration));
    assert!(!receipt.matches(&original, &"b".repeat(64)));
    assert!(!receipt.matches(
        &Command::Focus {
            request_id: ID.into()
        },
        &scope.configuration
    ));
}
#[test]
fn unconfigured_status_never_claims_a_saved_preference() {
    let mut status = Status {
        abi: ABI.into(),
        scope: scope(),
        journal: None,
        revision: 0,
        configured: false,
        preferences: Preferences::default(),
    };
    assert!(status.valid());
    status.preferences.optional_questions = true;
    assert!(!status.valid());
    status.configured = true;
    assert!(!status.valid());
    status.journal = Some(ID.into());
    status.revision = 1;
    assert!(status.valid());
}
#[test]
fn foreground_is_native_bounded_and_not_a_durable_receipt() {
    let original = Command::Focus {
        request_id: ID.into(),
    };
    let scope = scope();
    let mut reply = FocusReply {
        abi: ABI.into(),
        request_id: ID.into(),
        scope: scope.clone(),
        observation: NativeFocus {
            connection_id: ID.into(),
            revision: 1,
            state: FocusState::Foreground,
        },
        ttl_ms: FOCUS_TTL_MS,
    };
    assert!(reply.matches(&original, &scope));
    reply.ttl_ms += 1;
    assert!(!reply.matches(&original, &scope));
    reply.ttl_ms = FOCUS_TTL_MS;
    reply.observation.state = FocusState::Unknown;
    assert!(!reply.matches(&original, &scope));
    reply.ttl_ms = 0;
    assert!(reply.matches(&original, &scope));
    let mut other = scope.clone();
    other.service_epoch = "22345678-1234-4234-8234-123456789abc".into();
    assert!(!reply.matches(&original, &other));
    assert!(serde_json::from_value::<Receipt>(serde_json::to_value(&reply).unwrap()).is_err());
}
#[test]
fn missing_unjournaled_receipt_cannot_be_definitive_and_focus_cannot_query() {
    let mut query = Query {
        request_id: ID.into(),
        scope: scope(),
        original: configure(),
    };
    let mut lookup = Lookup {
        abi: ABI.into(),
        request_id: ID.into(),
        scope: scope(),
        original_request_id: ID.into(),
        fingerprint: query.original.fingerprint(&scope().configuration).unwrap(),
        receipt: None,
        definitive_not_applied: false,
    };
    assert!(lookup.matches(&query));
    lookup.definitive_not_applied = true;
    assert!(!lookup.matches(&query));
    if let Command::Configure { journal, .. } = &mut query.original {
        *journal = Some(ID.into());
    }
    lookup.fingerprint = query.original.fingerprint(&scope().configuration).unwrap();
    assert!(lookup.matches(&query));
    query.original = Command::Focus {
        request_id: ID.into(),
    };
    assert!(!query.valid());
}
