//! Compiled fork edition. User data uses the common Aurora namespace.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    Classic,
    Ftrlv,
}

pub const CURRENT: Edition = if cfg!(feature = "rlva-always-on") {
    Edition::Ftrlv
} else {
    Edition::Classic
};

impl Edition {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Ftrlv => "ftrlv",
        }
    }

    pub const fn always_on(self) -> bool {
        matches!(self, Self::Ftrlv)
    }
}
