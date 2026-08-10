use super::{runtime_coordination_unavailable_gateway_error, GatewayError};
use crate::state::{
    AccountConcurrencyKey, AccountProbeLease, AccountProbeOutcome, AccountWaitTicket, AppState,
    DownstreamConcurrencyLease, ProbeDecision, RuntimeCoordinationError,
};
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[cfg(test)]
use std::sync::Mutex;

const RENEWAL_INTERVAL: Duration = Duration::from_secs(30);
const PROBE_EXPIRY_MARGIN: Duration = Duration::from_secs(30);
const ACCOUNT_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(test)]
static PROBE_COMPLETION_FAILURE_TEST_UPSTREAM: Mutex<Option<String>> = Mutex::new(None);

#[cfg(test)]
pub(super) fn install_probe_completion_failure_test_hook(upstream_id: impl Into<String>) {
    let mut failure = PROBE_COMPLETION_FAILURE_TEST_UPSTREAM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(failure.is_none(), "probe completion failure already armed");
    *failure = Some(upstream_id.into());
}

#[cfg(test)]
fn take_probe_completion_failure_test_hook(upstream_id: &str) -> bool {
    let mut failure = PROBE_COMPLETION_FAILURE_TEST_UPSTREAM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if failure.as_deref() != Some(upstream_id) {
        return false;
    }
    failure.take();
    true
}

pub(super) struct AccountRecoverySession {
    state: AppState,
    request_id: String,
    downstream_lease: DownstreamConcurrencyLease,
    deadline: Instant,
    recovery_deadline: Option<Instant>,
    tickets: Vec<AccountWaitTicket>,
    probe: Option<AccountProbeLease>,
    probe_renewal: Option<ProbeRenewal>,
    waiting_marked: bool,
    waited: Duration,
    rounds: u32,
    max_rounds: u32,
    finished: bool,
}

pub(super) enum AccountAdmission {
    Ordinary,
    Deferred { retry_after: Duration },
    Probe(AccountProbeLease),
}

#[derive(Clone, Copy)]
enum ProbeOwnership {
    ActiveUntil(Instant),
    Failed,
}

struct ProbeRenewal {
    stop: watch::Sender<bool>,
    ownership: watch::Receiver<ProbeOwnership>,
    task: JoinHandle<()>,
}

impl ProbeRenewal {
    fn start(state: AppState, lease: AccountProbeLease, probe_ttl: Duration) -> Self {
        let safe_for = probe_ttl.saturating_sub(PROBE_EXPIRY_MARGIN);
        let (stop, mut stop_receiver) = watch::channel(false);
        let (ownership_sender, ownership) =
            watch::channel(ProbeOwnership::ActiveUntil(Instant::now() + safe_for));
        let task = tokio::spawn(async move {
            let mut next_renewal = Instant::now() + RENEWAL_INTERVAL;
            loop {
                tokio::select! {
                    changed = stop_receiver.changed() => {
                        if changed.is_err() || *stop_receiver.borrow() {
                            return;
                        }
                    }
                    _ = tokio::time::sleep_until(next_renewal) => {
                        if state.renew_account_probe(&lease).await.is_err() {
                            ownership_sender.send_replace(ProbeOwnership::Failed);
                            return;
                        }
                        ownership_sender.send_replace(ProbeOwnership::ActiveUntil(
                            Instant::now() + safe_for,
                        ));
                        next_renewal = Instant::now() + RENEWAL_INTERVAL;
                    }
                }
            }
        });
        Self {
            stop,
            ownership,
            task,
        }
    }

    async fn wait_for_failure(&mut self) {
        loop {
            let ownership = *self.ownership.borrow_and_update();
            match ownership {
                ProbeOwnership::Failed => return,
                ProbeOwnership::ActiveUntil(deadline) => {
                    tokio::select! {
                        changed = self.ownership.changed() => {
                            if changed.is_err() {
                                return;
                            }
                        }
                        _ = tokio::time::sleep_until(deadline) => return,
                    }
                }
            }
        }
    }

    async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }

    fn abort(self) {
        self.task.abort();
    }
}

impl AccountRecoverySession {
    pub fn new(
        state: AppState,
        request_id: String,
        downstream_lease: DownstreamConcurrencyLease,
        deadline: Instant,
        max_rounds: u32,
    ) -> Self {
        Self {
            state,
            request_id,
            downstream_lease,
            deadline,
            recovery_deadline: None,
            tickets: Vec::new(),
            probe: None,
            probe_renewal: None,
            waiting_marked: false,
            waited: Duration::ZERO,
            rounds: 0,
            max_rounds: max_rounds.max(1),
            finished: false,
        }
    }

    pub async fn wait_for_account(
        &mut self,
        account: AccountConcurrencyKey,
    ) -> Result<AccountAdmission, GatewayError> {
        if self
            .probe
            .as_ref()
            .is_some_and(|probe| probe.account == account)
        {
            return Ok(AccountAdmission::Probe(
                self.probe
                    .as_ref()
                    .expect("matching account probe disappeared")
                    .clone(),
            ));
        }
        if self.probe.is_some() || self.tickets.iter().any(|ticket| ticket.account == account) {
            return Ok(AccountAdmission::Deferred {
                retry_after: self.recovery_retry_after(),
            });
        }

        let Some(ticket) = self
            .state
            .register_account_waiter_for_downstream_lease_if_saturated(
                &account,
                &self.request_id,
                &self.downstream_lease,
            )
            .await
            .map_err(coordination_error)?
        else {
            return Ok(AccountAdmission::Ordinary);
        };
        self.tickets.push(ticket);
        self.refresh_recovery_deadline(&account).await?;
        let retry_after = self.recovery_retry_after();
        if !self.waiting_marked {
            if let Err(error) = self
                .state
                .mark_downstream_waiting(&self.downstream_lease)
                .await
            {
                return Err(coordination_error(error));
            }
            self.waiting_marked = true;
        }
        Ok(AccountAdmission::Deferred { retry_after })
    }

    async fn wait_for_ticket(
        &mut self,
        selected: AccountWaitTicket,
    ) -> Result<AccountAdmission, GatewayError> {
        let mut next_renewal = Instant::now() + RENEWAL_INTERVAL;
        loop {
            if Instant::now() >= self.deadline || self.rounds >= self.max_rounds {
                return Err(self.account_budget_exhausted());
            }
            match self
                .state
                .try_acquire_account_probe_for_downstream_lease(&selected, &self.downstream_lease)
                .await
                .map_err(coordination_error)?
            {
                ProbeDecision::Granted(lease) => {
                    self.tickets
                        .retain(|ticket| ticket.registration_token != selected.registration_token);
                    self.probe = Some(lease.clone());
                    self.waiting_marked = false;
                    self.rounds = self.rounds.saturating_add(1);
                    let probe_ttl = Duration::from_secs(
                        self.state
                            .config
                            .upstream_response_header_timeout_seconds
                            .saturating_add(60),
                    );
                    self.probe_renewal = Some(ProbeRenewal::start(
                        self.state.clone(),
                        lease.clone(),
                        probe_ttl,
                    ));
                    return Ok(AccountAdmission::Probe(lease));
                }
                ProbeDecision::Wait { retry_after } => {
                    let now = Instant::now();
                    let wake_at = (now + retry_after.min(ACCOUNT_POLL_INTERVAL))
                        .min(next_renewal)
                        .min(self.deadline);
                    tokio::time::sleep_until(wake_at).await;
                    self.waited = self
                        .waited
                        .saturating_add(Instant::now().saturating_duration_since(now));
                    if Instant::now() >= next_renewal {
                        for ticket in self.tickets.clone() {
                            self.state
                                .renew_account_waiter(&ticket)
                                .await
                                .map_err(coordination_error)?;
                        }
                        next_renewal = Instant::now() + RENEWAL_INTERVAL;
                    }
                }
            }
        }
    }

    pub async fn wait_for_probe_interruption(&mut self) -> GatewayError {
        let started = Instant::now();
        let deadline = self.deadline;
        let budget_exhausted = match self.probe_renewal.as_mut() {
            Some(renewal) => {
                tokio::select! {
                    _ = renewal.wait_for_failure() => false,
                    _ = tokio::time::sleep_until(deadline) => true,
                }
            }
            None => {
                tokio::time::sleep_until(deadline).await;
                true
            }
        };
        if budget_exhausted {
            self.waited = self
                .waited
                .saturating_add(Instant::now().saturating_duration_since(started));
            self.account_budget_exhausted()
        } else {
            runtime_coordination_unavailable_gateway_error()
        }
    }

    pub fn active_probe_account(&self) -> Option<&AccountConcurrencyKey> {
        self.probe.as_ref().map(|probe| &probe.account)
    }

    pub fn has_pending_recovery(&self) -> bool {
        !self.tickets.is_empty() || self.probe.is_some()
    }

    pub async fn wait_for_pending_account(&mut self) -> Result<bool, GatewayError> {
        if self.probe.is_some() {
            return Ok(true);
        }
        let Some((_, ticket)) = self
            .tickets
            .iter()
            .enumerate()
            .min_by_key(|(index, ticket)| (ticket.registered_at_ms, *index))
            .map(|(index, ticket)| (index, ticket.clone()))
        else {
            return Ok(false);
        };
        Ok(matches!(
            self.wait_for_ticket(ticket).await?,
            AccountAdmission::Probe(_)
        ))
    }

    pub async fn complete_attempt(
        &mut self,
        account: &AccountConcurrencyKey,
        outcome: AccountProbeOutcome,
    ) -> Result<(), GatewayError> {
        #[cfg(test)]
        let inject_completion_failure =
            take_probe_completion_failure_test_hook(&account.upstream_id);
        if let Some(renewal) = self.probe_renewal.take() {
            renewal.stop().await;
        }
        if let Some(probe) = self.probe.clone() {
            if &probe.account != account {
                self.state
                    .finish_account_probe(&probe, AccountProbeOutcome::Cancelled)
                    .await
                    .map_err(coordination_error)?;
                self.probe = None;
                return Err(coordination_error(RuntimeCoordinationError));
            }
            self.state
                .finish_account_probe(&probe, outcome)
                .await
                .map_err(coordination_error)?;
            self.probe = None;
            #[cfg(test)]
            if inject_completion_failure {
                return Err(coordination_error(RuntimeCoordinationError));
            }
        } else if let AccountProbeOutcome::ConcurrencyRejected { retry_after } = outcome {
            self.state
                .observe_account_concurrency(account, retry_after)
                .await
                .map_err(coordination_error)?;
        }

        if matches!(
            outcome,
            AccountProbeOutcome::ConcurrencyRejected { .. } | AccountProbeOutcome::AttemptFailed
        ) {
            self.refresh_recovery_deadline(account).await?;
        }

        match outcome {
            AccountProbeOutcome::ConcurrencyRejected { .. } => {
                if self
                    .state
                    .account_requires_recovery(account)
                    .await
                    .map_err(coordination_error)?
                {
                    self.enqueue(account).await?;
                }
            }
            AccountProbeOutcome::Accepted => {
                self.cancel_all_waiters().await?;
            }
            AccountProbeOutcome::AttemptFailed => {
                if self
                    .state
                    .account_requires_recovery(account)
                    .await
                    .map_err(coordination_error)?
                {
                    self.enqueue(account).await?;
                }
            }
            AccountProbeOutcome::Cancelled => {}
        }
        Ok(())
    }

    async fn enqueue(&mut self, account: &AccountConcurrencyKey) -> Result<(), GatewayError> {
        if self.tickets.iter().any(|ticket| &ticket.account == account) {
            return Ok(());
        }
        let ticket = self
            .state
            .register_account_waiter_for_downstream_lease_if_saturated(
                account,
                &self.request_id,
                &self.downstream_lease,
            )
            .await
            .map_err(coordination_error)?;
        let Some(ticket) = ticket else {
            return Ok(());
        };
        self.tickets.push(ticket);
        if !self.waiting_marked {
            if let Err(error) = self
                .state
                .mark_downstream_waiting(&self.downstream_lease)
                .await
            {
                return Err(coordination_error(error));
            }
            self.waiting_marked = true;
        }
        Ok(())
    }

    async fn cancel_all_waiters(&mut self) -> Result<(), GatewayError> {
        let mut failed = false;
        let mut retained = Vec::new();
        for ticket in std::mem::take(&mut self.tickets) {
            if self.state.cancel_account_waiter(&ticket).await.is_err() {
                failed = true;
                retained.push(ticket);
            }
        }
        self.tickets = retained;
        if self.waiting_marked {
            if self
                .state
                .unmark_downstream_waiting(&self.downstream_lease)
                .await
                .is_err()
            {
                failed = true;
            } else {
                self.waiting_marked = false;
            }
        }
        if failed {
            Err(coordination_error(RuntimeCoordinationError))
        } else {
            Ok(())
        }
    }

    pub async fn finish(&mut self) -> Result<(), GatewayError> {
        if self.finished {
            return Ok(());
        }
        let mut failed = false;
        if let Some(renewal) = self.probe_renewal.take() {
            renewal.stop().await;
        }
        if let Some(probe) = self.probe.as_ref() {
            if self
                .state
                .finish_account_probe(probe, AccountProbeOutcome::Cancelled)
                .await
                .is_err()
            {
                failed = true;
            } else {
                self.probe = None;
            }
        }
        failed |= self.cancel_all_waiters().await.is_err();
        if failed {
            Err(coordination_error(RuntimeCoordinationError))
        } else {
            self.finished = true;
            Ok(())
        }
    }

    pub fn waited(&self) -> Duration {
        self.waited
    }

    pub fn rounds(&self) -> u32 {
        self.rounds
    }

    async fn refresh_recovery_deadline(
        &mut self,
        account: &AccountConcurrencyKey,
    ) -> Result<(), GatewayError> {
        let retry_after = self
            .state
            .account_recovery_retry_after(account)
            .await
            .map_err(coordination_error)?;
        let Some(deadline) = Instant::now().checked_add(retry_after) else {
            return Err(coordination_error(RuntimeCoordinationError));
        };
        self.recovery_deadline = Some(
            self.recovery_deadline
                .map_or(deadline, |current| current.max(deadline)),
        );
        Ok(())
    }

    fn account_budget_exhausted(&self) -> GatewayError {
        GatewayError::ConcurrencyFull {
            message: "upstream account concurrency recovery budget exhausted".to_string(),
            retry_after: Some(self.recovery_retry_after()),
            upstream_status: None,
        }
    }

    fn recovery_retry_after(&self) -> Duration {
        self.recovery_deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
            .max(Duration::from_secs(1))
    }
}

impl Drop for AccountRecoverySession {
    fn drop(&mut self) {
        if self.finished
            || (self.tickets.is_empty() && self.probe.is_none() && !self.waiting_marked)
        {
            return;
        }
        if let Some(renewal) = self.probe_renewal.take() {
            renewal.abort();
        }
        let state = self.state.clone();
        let tickets = std::mem::take(&mut self.tickets);
        let probe = self.probe.take();
        let lease = self.downstream_lease.clone();
        let waiting_marked = self.waiting_marked;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(probe) = probe {
                    let _ = state
                        .finish_account_probe(&probe, AccountProbeOutcome::Cancelled)
                        .await;
                }
                for ticket in tickets {
                    let _ = state.cancel_account_waiter(&ticket).await;
                }
                if waiting_marked {
                    let _ = state.unmark_downstream_waiting(&lease).await;
                }
            });
        }
    }
}

fn coordination_error(_: RuntimeCoordinationError) -> GatewayError {
    runtime_coordination_unavailable_gateway_error()
}
