use crate::{seconds, session_locked, Idle, IdleSource, Sample};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString, CFType};
use objc2_core_graphics::{
    CGEventSource, CGEventSourceStateID, CGEventType, CGSessionCopyCurrentDictionary,
};

pub(super) fn sample() -> Sample {
    // Combined session input is the public Quartz aggregate used by the old Qt
    // source too. No event tap, Accessibility grant, or input contents are read.
    let idle = seconds(CGEventSource::seconds_since_last_event_type(
        CGEventSourceStateID::CombinedSessionState,
        CGEventType(u32::MAX), // kCGAnyInputEventType
    ))
    .map(|elapsed| Idle {
        elapsed,
        source: IdleSource::MacEventSource,
        since_input: true,
    });
    let locked = CGSessionCopyCurrentDictionary().and_then(|dictionary| {
        // SAFETY: CGSessionCopyCurrentDictionary returns a CF dictionary of CF
        // objects. This only restores its erased container generics; values are
        // still dynamically checked as CFBoolean before accessing them.
        let dictionary = unsafe { dictionary.cast_unchecked::<CFType, CFType>() };
        lock_state(dictionary)
    });
    Sample { idle, locked }
}

// CGSession.h: kCGSessionOnConsoleKey expands to this double-S string.
fn lock_state(dictionary: &CFDictionary<CFType, CFType>) -> Option<bool> {
    // WindowServer may omit the lock flag for an unlocked session (the Qt
    // source also treats that omission as false). Require positive on-console
    // evidence before declaring it unlocked; a missing dictionary/console or a
    // present but wrongly typed lock flag still stays unknown.
    session_locked(
        boolean(dictionary, "CGSSessionScreenIsLocked", Some(false)),
        boolean(dictionary, "kCGSSessionOnConsoleKey", None),
    )
}

fn boolean(
    dictionary: &CFDictionary<CFType, CFType>,
    key: &str,
    missing: Option<bool>,
) -> Option<bool> {
    let key = CFString::from_str(key);
    match dictionary.get(&key) {
        Some(value) => value.downcast_ref::<CFBoolean>().map(CFBoolean::value),
        None => missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_non_boolean_session_fields_are_unknown() {
        let key = CFString::from_str("CGSSessionScreenIsLocked");
        let text = CFString::from_str("false");
        let dictionary = CFDictionary::<CFType, CFType>::from_slices(&[&key], &[&text]);
        assert_eq!(
            boolean(&dictionary, "CGSSessionScreenIsLocked", Some(false)),
            None
        );
        assert_eq!(boolean(&dictionary, "kCGSSessionOnConsoleKey", None), None);
        assert_eq!(lock_state(&dictionary), None);
        assert_eq!(lock_state(&CFDictionary::empty()), None);
    }

    #[test]
    fn unlocked_console_and_fast_user_switch_are_distinguished() {
        let console = CFString::from_str("kCGSSessionOnConsoleKey");
        let locked = CFString::from_str("CGSSessionScreenIsLocked");
        let dictionary =
            CFDictionary::<CFType, CFType>::from_slices(&[&console], &[CFBoolean::new(true)]);
        assert_eq!(lock_state(&dictionary), Some(false));
        let dictionary =
            CFDictionary::<CFType, CFType>::from_slices(&[&console], &[CFBoolean::new(false)]);
        assert_eq!(lock_state(&dictionary), Some(true));
        let dictionary = CFDictionary::<CFType, CFType>::from_slices(
            &[&console, &locked],
            &[CFBoolean::new(true), CFBoolean::new(true)],
        );
        assert_eq!(lock_state(&dictionary), Some(true));
    }
}
