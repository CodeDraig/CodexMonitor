use super::*;

impl Store {
    /// Correlate and save evidence with its instance transition in one transaction.
    /// A busy failure can be retried without losing an event or applying half a transition.
    pub(crate) fn record_event(&mut self, context: &RunContext, event: &Value) -> Result<bool> {
        let Some(turn) = event
            .pointer("/params/turnId")
            .or_else(|| event.pointer("/params/turn/id"))
            .and_then(Value::as_str)
        else {
            return Ok(false);
        };
        let method = event["method"].as_str().unwrap_or_default();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let mut instance: RunInstance = decode(
            tx.query_row(
                "SELECT payload FROM instances WHERE id=?",
                [&context.instance_id],
                |r| r.get(0),
            )
            .map_err(error)?,
        )?;
        if instance.run_id != context.run_id {
            return Err("Run context does not match instance".into());
        }
        if instance
            .initial_turn_id
            .as_deref()
            .is_some_and(|id| id != turn)
            || (instance.initial_turn_id.is_none()
                && !matches!(instance.submission_outcome, SubmissionOutcome::Submitting))
        {
            return Ok(false);
        }
        let key = if method == "item/completed" {
            format!(
                "item:{}",
                event
                    .pointer("/params/item/id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            )
        } else {
            method.to_string()
        };
        tx.execute("INSERT INTO evidence VALUES(?,?,?,?) ON CONFLICT(instance_id,turn_id,event_key) DO UPDATE SET payload=excluded.payload",
            params![instance.id, turn, key, encode(event)?]).map_err(error)?;
        instance.initial_turn_id = Some(turn.into());
        instance.submission_outcome = SubmissionOutcome::Accepted;
        if method == "turn/completed" {
            instance.execution_outcome = super::super::capture::outcome(
                event
                    .pointer("/params/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
            );
        } else if instance.execution_outcome == ExecutionOutcome::NotStarted {
            instance.execution_outcome = ExecutionOutcome::Running;
        }
        tx.execute(
            "UPDATE instances SET payload=? WHERE id=?",
            params![encode(&instance)?, instance.id],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(true)
    }
}
