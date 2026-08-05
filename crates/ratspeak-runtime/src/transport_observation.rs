//! Bounded, read-only observations of Reticulum transport state.
//!
//! These helpers never request, drop, suppress, or synthesize paths. A missed
//! deadline means only that no fresh observation was available to the caller.

use std::time::Duration;

use rns_runtime::reticulum::ReticulumHandle;
use rns_transport::messages::{
    PathTableRpcEntry, TransportMessage, TransportQuery, TransportQueryResponse,
};
use tokio::sync::mpsc;

pub const PATH_TABLE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(1);

pub async fn local_path_table(
    transport_tx: &mpsc::Sender<TransportMessage>,
) -> Option<Vec<PathTableRpcEntry>> {
    local_path_table_with_timeout(transport_tx, PATH_TABLE_OBSERVATION_TIMEOUT).await
}

async fn local_path_table_with_timeout(
    transport_tx: &mpsc::Sender<TransportMessage>,
    timeout: Duration,
) -> Option<Vec<PathTableRpcEntry>> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let transaction = async {
        transport_tx
            .send(TransportMessage::Rpc {
                query: TransportQuery::GetPathTable,
                response_tx,
            })
            .await
            .ok()?;
        match response_rx.await.ok()? {
            TransportQueryResponse::PathTable(entries) => Some(entries),
            _ => None,
        }
    };

    match tokio::time::timeout(timeout, transaction).await {
        Ok(entries) => entries,
        Err(_) => {
            tracing::debug!(
                timeout_ms = timeout.as_millis() as u64,
                "path-table observation timed out"
            );
            None
        }
    }
}

pub async fn authoritative_path_table(handle: &ReticulumHandle) -> Option<Vec<PathTableRpcEntry>> {
    authoritative_path_table_with_timeout(handle, PATH_TABLE_OBSERVATION_TIMEOUT).await
}

async fn authoritative_path_table_with_timeout(
    handle: &ReticulumHandle,
    timeout: Duration,
) -> Option<Vec<PathTableRpcEntry>> {
    let query = handle.query_control(TransportQuery::GetPathTable);
    match tokio::time::timeout(timeout, query).await {
        Ok(Some(TransportQueryResponse::PathTable(entries))) => Some(entries),
        Ok(_) => None,
        Err(_) => {
            tracing::debug!(
                timeout_ms = timeout.as_millis() as u64,
                "authoritative path-table observation timed out"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_observation_deadline_includes_channel_submission() {
        let timeout = Duration::from_millis(40);
        let (tx, rx) = mpsc::channel::<TransportMessage>(1);
        tx.send(TransportMessage::Shutdown)
            .await
            .expect("prefill actor channel");

        let started = std::time::Instant::now();
        assert!(local_path_table_with_timeout(&tx, timeout).await.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(rx);
    }

    #[tokio::test]
    async fn local_observation_deadline_includes_response_wait() {
        let timeout = Duration::from_millis(40);
        let (tx, mut rx) = mpsc::channel::<TransportMessage>(1);
        let responder = tokio::spawn(async move {
            let TransportMessage::Rpc { response_tx, .. } =
                rx.recv().await.expect("path-table query")
            else {
                panic!("expected transport RPC");
            };
            std::future::pending::<()>().await;
            drop(response_tx);
        });

        assert!(local_path_table_with_timeout(&tx, timeout).await.is_none());
        responder.abort();
    }

    #[tokio::test]
    async fn local_observation_accepts_only_path_table_responses() {
        let (tx, mut rx) = mpsc::channel::<TransportMessage>(1);
        let responder = tokio::spawn(async move {
            let TransportMessage::Rpc { response_tx, .. } =
                rx.recv().await.expect("path-table query")
            else {
                panic!("expected transport RPC");
            };
            response_tx
                .send(TransportQueryResponse::IntResult(1))
                .expect("query response");
        });

        assert!(
            local_path_table_with_timeout(&tx, Duration::from_secs(1))
                .await
                .is_none()
        );
        responder.await.expect("responder task");
    }
}
