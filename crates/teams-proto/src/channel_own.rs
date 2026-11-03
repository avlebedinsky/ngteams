//! Sealed account-only channel preferences and folders. These operations name no message recipients and cannot
//! change a shared channel. The native route verifies the actor independently and sends each update only once.
use crate::{ChannelSectionWrite, OwnTeamView, Request};

pub trait ChannelOwn: Request + sealed::Sealed {
    /// The explicitly named account on an interest update; every other route is token-owned `ME` state.
    fn interest_account(&self) -> Option<&str> {
        None
    }
}
mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::OwnTeamView {}
    impl Sealed for crate::ChannelSectionWrite {}
}
impl ChannelOwn for ChannelSectionWrite {}
impl ChannelOwn for OwnTeamView {
    fn interest_account(&self) -> Option<&str> {
        match self {
            Self::ChannelInterest { own, .. } => Some(own),
            _ => None,
        }
    }
}
