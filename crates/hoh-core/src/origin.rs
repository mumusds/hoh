//! Host-observed approval authority (REQ-008). Never deserialized from a client command.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Origin {
    User,
    Marker,
    AncestorMarker,
    AttemptCgroup,
    #[default]
    Unknown,
}

impl Origin {
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::User => None,
            Self::Marker => Some("marker"),
            Self::AncestorMarker => Some("ancestor_marker"),
            Self::AttemptCgroup => Some("attempt_cgroup"),
            Self::Unknown => Some("origin_unknown"),
        }
    }
}
