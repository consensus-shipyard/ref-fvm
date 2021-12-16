use std::error::Error;
use std::{cell::Cell, sync::Mutex};

use derive_more::Display;
use fvm_shared::error::ExitCode::ErrPlaceholder;
use fvm_shared::{actor_error, address, encoding, error::ActorError, error::ExitCode};
use wasmtime::Trap;

use crate::kernel::blocks;

/// Execution result.
pub type Result<T> = std::result::Result<T, ExecutionError>;

#[derive(thiserror::Error, Debug)]
pub enum ExecutionError {
    #[error("{0:?}")]
    Actor(#[from] ActorError),
    #[error(transparent)]
    Syscall(#[from] SyscallError),
    #[error("{0:?}")]
    SystemError(#[from] anyhow::Error),
}

/// Represents an error from a syscall. It can optionally contain a
/// syscall-advised exit code for the kind of error that was raised.
/// We may want to add an optional source error here.
///
/// Automatic conversions from String are provided, with no advised exit code.
///
/// TODO Many usages of ActorError should migrate to this type.
#[derive(thiserror::Error, Debug)]
#[error("syscall error: {0} (exit_code={1:?})")]
pub struct SyscallError(pub String, pub Option<ExitCode>);

impl ExecutionError {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            ExecutionError::Actor(e) => e.exit_code(),
            ExecutionError::SystemError(_) => ExitCode::ErrPlaceholder, // same as fatal before
            ExecutionError::Syscall(SyscallError(_, exit_code)) => {
                exit_code.unwrap_or(ExitCode::ErrPlaceholder)
            }
        }
    }
}

impl From<String> for SyscallError {
    fn from(s: String) -> Self {
        SyscallError(s, None)
    }
}

impl From<&str> for SyscallError {
    fn from(s: &str) -> Self {
        SyscallError(s.to_owned(), None)
    }
}

impl From<encoding::Error> for ExecutionError {
    fn from(e: encoding::Error) -> Self {
        ExecutionError::SystemError(e.into())
    }
}

impl From<encoding::error::Error> for ExecutionError {
    fn from(e: encoding::error::Error) -> Self {
        ExecutionError::SystemError(e.into())
    }
}

impl From<blocks::BlockError> for ExecutionError {
    fn from(e: blocks::BlockError) -> Self {
        use blocks::BlockError::*;
        match e {
            Unreachable(..)
            | InvalidHandle(..)
            | InvalidMultihashSpec { .. }
            | InvalidCodec(..) => {
                ExecutionError::Actor(actor_error!(SysErrIllegalArgument; e.to_string()))
            }
            // TODO: Not quite the correct error but we don't have a better oen for now.
            TooManyBlocks => ExecutionError::Actor(actor_error!(SysErrIllegalActor; e.to_string())),
            MissingState(k) => ExecutionError::SystemError(anyhow::anyhow!("missing block: {}", k)),
        }
    }
}

impl From<ipld_hamt::Error> for ExecutionError {
    fn from(e: ipld_hamt::Error) -> Self {
        // TODO: box dyn error is pervasive..
        ExecutionError::SystemError(anyhow::anyhow!("{:?}", e))
    }
}

impl From<cid::Error> for ExecutionError {
    fn from(e: cid::Error) -> Self {
        ExecutionError::SystemError(e.into())
    }
}

impl From<address::Error> for ExecutionError {
    fn from(e: address::Error) -> Self {
        ExecutionError::SystemError(e.into())
    }
}

impl From<Box<dyn std::error::Error>> for ExecutionError {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        // TODO: make better
        ExecutionError::SystemError(anyhow::anyhow!(e.to_string()))
    }
}

// Here begins the I HATE EVERYTHING section.
//
// Alternatively, we could just stash the error in the kernel. But that gets a bit annoying as we'd
// have to add boilerplate everywhere to do that.

impl From<ExecutionError> for Trap {
    fn from(e: ExecutionError) -> Self {
        Trap::from(
            Box::new(ErrorEnvelope::wrap(e)) as Box<dyn std::error::Error + Send + Sync + 'static>
        )
    }
}

impl From<Trap> for ExecutionError {
    fn from(e: Trap) -> Self {
        use std::error::Error;
        // Do whatever we can to pull the original error back out (if it exists).
        e.source()
            .and_then(|e| e.downcast_ref::<ErrorEnvelope>())
            .and_then(|e| e.inner.lock().ok())
            .and_then(|mut e| e.take())
            .unwrap_or_else(|| ExecutionError::SystemError(e.into()))
    }
}

/// A super special secret error type for stapling an error to a trap in a way that allows us to
/// pull it back out.
///
/// BE VERY CAREFUL WITH THIS ERROR TYPE: Its source is self-referential.
#[derive(Display, Debug)]
#[display(fmt = "wrapping error")]
struct ErrorEnvelope {
    inner: Mutex<Option<ExecutionError>>,
}

impl ErrorEnvelope {
    fn wrap(e: ExecutionError) -> Self {
        Self {
            inner: Mutex::new(Some(e)),
        }
    }
}

impl std::error::Error for ErrorEnvelope {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self)
    }
}
