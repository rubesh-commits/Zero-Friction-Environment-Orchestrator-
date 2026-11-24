use crate::paths::iso8601_now;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Explicit lifecycle of the zero-friction pipeline. The CLI advances through
/// these stages so failures are attributable to a specific phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    Idle,
    Acquired,
    Analyzed,
    HostProbed,
    Provisioned,
    SandboxReady,
    Running,
    Finished,
    Failed,
}

impl PipelineStage {
    pub fn as_str(self) -> &'static str {
        match self {
            PipelineStage::Idle => "idle",
            PipelineStage::Acquired => "acquired",
            PipelineStage::Analyzed => "analyzed",
            PipelineStage::HostProbed => "host-probed",
            PipelineStage::Provisioned => "provisioned",
            PipelineStage::SandboxReady => "sandbox-ready",
            PipelineStage::Running => "running",
            PipelineStage::Finished => "finished",
            PipelineStage::Failed => "failed",
        }
    }

    pub fn allowed_next(self) -> &'static [PipelineStage] {
        use PipelineStage::*;
        match self {
            Idle => &[Acquired, Failed],
            Acquired => &[Analyzed, Failed],
            Analyzed => &[HostProbed, Failed],
            HostProbed => &[Provisioned, Failed],
            Provisioned => &[SandboxReady, Failed],
            SandboxReady => &[Running, Finished, Failed],
            Running => &[Finished, Failed],
            Finished | Failed => &[],
        }
    }
}

impl fmt::Display for PipelineStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transition {
    pub stage: PipelineStage,
    pub at: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PipelineState {
    stage: PipelineStage,
    history: Vec<Transition>,
}

impl Default for PipelineState {
    fn default() -> Self {
        Self {
            stage: PipelineStage::Idle,
            history: vec![Transition {
                stage: PipelineStage::Idle,
                at: iso8601_now(),
                note: None,
            }],
        }
    }
}

impl PipelineState {
    pub fn stage(&self) -> PipelineStage {
        self.stage
    }

    pub fn history(&self) -> &[Transition] {
        &self.history
    }

    /// Advance to the next stage, enforcing the declared transitions. This
    /// turns accidental phase skips into immediate, testable errors.
    pub fn advance(
        &mut self,
        next: PipelineStage,
        note: impl Into<Option<String>>,
    ) -> crate::Result<()> {
        if !self.stage.allowed_next().contains(&next) {
            return Err(crate::Error::Message(format!(
                "invalid pipeline transition: {} -> {}",
                self.stage, next
            )));
        }
        self.stage = next;
        self.history.push(Transition {
            stage: next,
            at: iso8601_now(),
            note: note.into(),
        });
        Ok(())
    }

    pub fn fail(&mut self, note: impl Into<String>) {
        if self.stage != PipelineStage::Failed {
            self.stage = PipelineStage::Failed;
            self.history.push(Transition {
                stage: PipelineStage::Failed,
                at: iso8601_now(),
                note: Some(note.into()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_is_enforced() {
        let mut state = PipelineState::default();
        for stage in [
            PipelineStage::Acquired,
            PipelineStage::Analyzed,
            PipelineStage::HostProbed,
            PipelineStage::Provisioned,
            PipelineStage::SandboxReady,
            PipelineStage::Running,
            PipelineStage::Finished,
        ] {
            state.advance(stage, None).unwrap();
        }
        assert_eq!(state.stage(), PipelineStage::Finished);
        assert_eq!(state.history().len(), 8);
    }

    #[test]
    fn skipping_a_phase_is_rejected() {
        let mut state = PipelineState::default();
        assert!(state.advance(PipelineStage::Running, None).is_err());
        state.advance(PipelineStage::Acquired, None).unwrap();
        assert!(state.advance(PipelineStage::Running, None).is_err());
    }
}