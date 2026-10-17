//! `hutan recovery` and `hutan recovery ack`: what the last daemon restart did to each session
//! (protocol 0.26, capability `session.recovery`, `docs/protocol.md` § Recovery).

use hutan_client::recovery;
use hutan_protocol::messages::{Request, SessionList};
use hutan_protocol::recovery::{RecoveryAck, RecoveryAcked};

use super::Ctx;
use crate::args::{RecoveryAckArgs, RecoveryArgs, RecoveryCommand};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn recovery(&self, args: RecoveryArgs) -> Result<(), CliError> {
        match args.command {
            None => self.recovery_summary().await,
            Some(RecoveryCommand::Ack(args)) => self.recovery_ack(args).await,
        }
    }

    async fn recovery_summary(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let mut list = self.session_list(&client, None).await?;
        list.sessions.sort_by_key(|s| s.created_at_ms);
        // `--json` has every session that carries a recovery, history-only ones included.
        let summary = recovery::summary(&list.sessions, recovery::clock_text);
        list.sessions.retain(|s| s.recovery.is_some());
        let shown: SessionList = list;
        self.print(&shown, || match &summary {
            Some(summary) => summary.text(),
            None => "no session was changed by a restart, or all were dismissed".to_owned(),
        })
    }

    async fn recovery_ack(&self, args: RecoveryAckArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session_id = match &args.target {
            Some(target) => Some(self.resolve(&client, target).await?.id),
            None if args.all => None,
            None => {
                return Err(CliError::Usage("name a session, or pass --all".to_owned()));
            }
        };
        let acked: RecoveryAcked = self
            .call(
                &client,
                Request::SessionRecoveryAck(RecoveryAck { session_id }),
            )
            .await?;
        self.print(&acked, || match acked.sessions.len() {
            0 => "nothing to dismiss".to_owned(),
            1 => format!("dismissed {}", acked.sessions[0]),
            count => format!("dismissed {count} sessions"),
        })
    }
}
