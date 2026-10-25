//! Who may edit: the user or Claude, in turns.
//!
//! - Claude claims the mode with `edit.begin` and releases it with `edit.end`. The claim is
//!   granted only after the user has made no change for [`Gate::QUIET`]; otherwise it is refused
//!   with a retry hint. While Claude holds it, edits from the UI are refused.
//! - A control-channel edit without a claim passes the same quiet check, is its own history
//!   entry, and holds the turn for [`Gate::HOLD`] after it: the UI shows Claude at work while
//!   one-off commands come in. The hold is a signal, not a lock: an edit of the user's ends it
//!   (and Claude waits for quiet again), and it ends by itself when the commands stop.
//! - The claim lapses after [`Gate::TIMEOUT`] without a command from Claude, and the UI's "take
//!   over" ends it at once, so a crashed Claude session cannot lock the user out.
//! - External edits of the project file wait for the same quiet period while the user edits.
//!
//! Pure state with explicit times, so it is tested without sleeping.

use std::time::{Duration, Instant};

use penumbra_core::{Error, GroupId};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    User,
    /// Claude edits. `group` is `Some` inside an `edit.begin` claim (all edits join it) and
    /// `None` for the short hold that a one-off edit starts by itself (each edit its own entry).
    Claude {
        group: Option<GroupId>,
        label: String,
        last: Instant,
    },
}

#[derive(Clone, Debug)]
pub struct Gate {
    mode: Mode,
    last_user_edit: Option<Instant>,
}

/// The editing mode as reported by `edit.status` and shown in the UI.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GateStatus {
    /// `"user"` or `"claude"`.
    pub mode: &'static str,
    /// With mode `"claude"`: the turn comes from one-off commands (a hold the user's own edit
    /// ends), not from an `edit.begin` claim (which refuses the user's edits).
    pub hold: bool,
    /// What Claude said it is doing (`edit.begin`'s label).
    pub label: Option<String>,
    /// Seconds until Claude's claim lapses without another command.
    pub expires_in: Option<f64>,
    /// Seconds since the user last edited; `None` if never.
    pub user_idle: Option<f64>,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    /// How long the user must have been idle before Claude (or a file edit) gets a turn.
    pub const QUIET: Duration = Duration::from_secs(3);
    /// A claim without a command for this long lapses.
    pub const TIMEOUT: Duration = Duration::from_secs(120);
    /// How long one-off edits from the control channel keep the turn after the last one. So the
    /// UI shows Claude at work (blue) while commands come in one by one, and the user does not
    /// edit between two of them; the hold ends by itself when the commands stop.
    pub const HOLD: Duration = Duration::from_secs(5);

    pub fn new() -> Self {
        Self { mode: Mode::User, last_user_edit: None }
    }

    /// How long the current Claude mode lives without a command: a claim 120 s, a hold 5 s.
    fn lifetime(&self) -> Duration {
        match &self.mode {
            Mode::Claude { group: Some(_), .. } => Self::TIMEOUT,
            _ => Self::HOLD,
        }
    }

    fn expire(&mut self, now: Instant) {
        if let Mode::Claude { group, last, .. } = &self.mode
            && now.saturating_duration_since(*last) >= self.lifetime()
        {
            if group.is_some() {
                log::info!(
                    "Claude's editing claim lapsed after {} s without a command",
                    Self::TIMEOUT.as_secs()
                );
            }
            self.mode = Mode::User;
        }
    }

    fn user_quiet(&self, now: Instant) -> Result<(), Error> {
        let Some(last) = self.last_user_edit else { return Ok(()) };
        let idle = now.saturating_duration_since(last);
        if idle >= Self::QUIET {
            return Ok(());
        }
        Err(Error::new(format!(
            "the user is editing (last change {:.1} s ago); retry in {:.1} s",
            idle.as_secs_f64(),
            Self::QUIET.saturating_sub(idle).as_secs_f64()
        )))
    }

    pub fn claude_holds(&mut self, now: Instant) -> bool {
        self.expire(now);
        matches!(self.mode, Mode::Claude { .. })
    }

    /// `edit.begin`: claim the mode for Claude. `group` is used for all of Claude's edits until
    /// `edit.end`, so they become one history entry. Claiming again while holding the claim
    /// keeps the group and only refreshes the label. A claim during a hold needs no quiet: Claude
    /// has the turn already.
    pub fn begin(&mut self, now: Instant, label: &str, group: GroupId) -> Result<GroupId, Error> {
        self.expire(now);
        if let Mode::Claude { group: held, label: l, last } = &mut self.mode {
            *last = now;
            if !label.is_empty() {
                *l = label.to_string();
            }
            return Ok(*held.get_or_insert(group));
        }
        self.user_quiet(now)?;
        self.mode = Mode::Claude { group: Some(group), label: label.to_string(), last: now };
        Ok(group)
    }

    /// `edit.end`. Returns whether Claude held the claim.
    pub fn end(&mut self, now: Instant) -> bool {
        let held = self.claude_holds(now);
        self.mode = Mode::User;
        held
    }

    /// The UI's "take over": ends Claude's claim at once.
    pub fn take_over(&mut self) {
        if matches!(self.mode, Mode::Claude { .. }) {
            log::info!("the user took over editing");
        }
        self.mode = Mode::User;
    }

    /// Before an edit from the control channel. `Some(group)` inside a claim (the edit joins it),
    /// `None` for a one-off edit (the caller uses a fresh group). A one-off edit starts or
    /// extends the hold: Claude keeps the turn for [`Gate::HOLD`] after it.
    pub fn claude_edit(&mut self, now: Instant) -> Result<Option<GroupId>, Error> {
        self.expire(now);
        if let Mode::Claude { group, last, .. } = &mut self.mode {
            *last = now;
            return Ok(*group);
        }
        self.user_quiet(now)?;
        self.mode = Mode::Claude { group: None, label: String::new(), last: now };
        Ok(None)
    }

    /// The label of the claim Claude holds, when it has one: the name of the claim's history
    /// entry. Call after [`Gate::claude_edit`], which lets a lapsed claim go.
    pub fn claim_label(&self) -> Option<&str> {
        match &self.mode {
            Mode::Claude { label, .. } if !label.is_empty() => Some(label),
            _ => None,
        }
    }

    /// Before an edit from the UI, OSC, a key or the 3D mouse. Refused while Claude holds a
    /// claim; a hold ends instead (live control must keep working during a show while Claude's
    /// one-off commands come in), and Claude's next command waits for quiet again.
    pub fn user_edit(&mut self, now: Instant) -> Result<(), Error> {
        self.expire(now);
        match &self.mode {
            Mode::Claude { group: Some(_), label, .. } => {
                let doing = if label.is_empty() { String::new() } else { format!(" ({label})") };
                return Err(Error::new(format!("Claude is editing{doing}; use \"take over\" to edit now")));
            }
            Mode::Claude { group: None, .. } => self.mode = Mode::User,
            Mode::User => {}
        }
        self.last_user_edit = Some(now);
        Ok(())
    }

    /// Whether an external edit of the project file may be merged now.
    pub fn file_merge_allowed(&mut self, now: Instant) -> bool {
        self.claude_holds(now) || self.user_quiet(now).is_ok()
    }

    pub fn status(&mut self, now: Instant) -> GateStatus {
        self.expire(now);
        let user_idle = self.last_user_edit.map(|t| now.saturating_duration_since(t).as_secs_f64());
        match &self.mode {
            Mode::User => GateStatus { mode: "user", hold: false, label: None, expires_in: None, user_idle },
            Mode::Claude { group, label, last } => GateStatus {
                mode: "claude",
                hold: group.is_none(),
                label: (!label.is_empty()).then(|| label.clone()),
                expires_in: Some(
                    self.lifetime().saturating_sub(now.saturating_duration_since(*last)).as_secs_f64(),
                ),
                user_idle,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn claude_waits_for_three_quiet_seconds() {
        let t0 = Instant::now();
        let mut g = Gate::new();
        g.user_edit(t0).unwrap();
        let e = g.begin(t0 + S, "x", GroupId(1)).unwrap_err();
        assert!(e.0.contains("retry in 2.0 s"), "{e}");
        assert!(g.claude_edit(t0 + 2 * S).is_err());
        assert!(!g.file_merge_allowed(t0 + 2 * S));
        assert_eq!(g.begin(t0 + 3 * S, "x", GroupId(1)), Ok(GroupId(1)));
    }

    #[test]
    fn a_claim_blocks_the_ui_and_groups_claudes_edits() {
        let t0 = Instant::now();
        let mut g = Gate::new();
        assert_eq!(g.begin(t0, "build a scene", GroupId(5)), Ok(GroupId(5)));
        assert!(g.user_edit(t0 + S).unwrap_err().0.contains("build a scene"));
        assert_eq!(g.claude_edit(t0 + S), Ok(Some(GroupId(5))));
        // Claiming again keeps the group.
        assert_eq!(g.begin(t0 + S, "", GroupId(6)), Ok(GroupId(5)));
        assert!(g.file_merge_allowed(t0 + S));
        assert!(g.end(t0 + 2 * S));
        assert!(!g.end(t0 + 2 * S));
        assert!(g.user_edit(t0 + 2 * S).is_ok());
        // Without a claim, one command is its own entry.
        assert_eq!(g.claude_edit(t0 + 5 * S), Ok(None));
    }

    #[test]
    fn one_off_edits_hold_the_turn_for_five_seconds() {
        let t0 = Instant::now();
        let mut g = Gate::new();
        assert_eq!(g.claude_edit(t0), Ok(None));
        let s = g.status(t0 + S);
        assert_eq!((s.mode, s.hold, s.label, s.expires_in), ("claude", true, None, Some(4.0)));
        // The user's edit (UI, OSC, keys) passes during a hold and ends it; Claude then waits.
        assert!(g.user_edit(t0 + S).is_ok());
        assert_eq!(g.status(t0 + S).mode, "user");
        assert!(g.claude_edit(t0 + 2 * S).is_err());
        // Each further edit is its own entry and extends the hold.
        assert_eq!(g.claude_edit(t0 + 4 * S), Ok(None));
        assert!(g.claude_holds(t0 + 8 * S));
        assert!(!g.claude_holds(t0 + 9 * S));
        assert!(g.user_edit(t0 + 9 * S).is_ok());
        // A claim during a hold needs no quiet and groups from then on.
        assert_eq!(g.claude_edit(t0 + 12 * S), Ok(None));
        assert_eq!(g.begin(t0 + 13 * S, "tuning", GroupId(7)), Ok(GroupId(7)));
        assert_eq!(g.claude_edit(t0 + 14 * S), Ok(Some(GroupId(7))));
        assert!(g.user_edit(t0 + 14 * S).is_err(), "a claim refuses the user's edits");
        let s = g.status(t0 + 14 * S);
        assert_eq!((s.hold, s.expires_in), (false, Some(120.0)));
    }

    #[test]
    fn the_claim_lapses_and_can_be_taken_over() {
        let t0 = Instant::now();
        let mut g = Gate::new();
        g.begin(t0, "", GroupId(1)).unwrap();
        g.claude_edit(t0 + 100 * S).unwrap();
        // 120 s after the last command, not after the claim.
        assert!(g.claude_holds(t0 + 200 * S));
        assert!(!g.claude_holds(t0 + 220 * S));
        assert!(g.user_edit(t0 + 220 * S).is_ok());

        g.begin(t0 + 300 * S, "", GroupId(2)).unwrap();
        g.take_over();
        assert!(g.user_edit(t0 + 300 * S).is_ok());
        assert_eq!(g.status(t0 + 300 * S).mode, "user");
    }

    #[test]
    fn status_reports_the_claim() {
        let t0 = Instant::now();
        let mut g = Gate::new();
        g.begin(t0, "tuning", GroupId(1)).unwrap();
        let s = g.status(t0 + 20 * S);
        assert_eq!(s.mode, "claude");
        assert_eq!(s.label.as_deref(), Some("tuning"));
        assert_eq!(s.expires_in, Some(100.0));
    }
}
