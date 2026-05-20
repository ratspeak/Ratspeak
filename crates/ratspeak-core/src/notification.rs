#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeNotificationKind {
    Message,
    Game,
    Call,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeNotification {
    pub kind: NativeNotificationKind,
    pub title: String,
    pub body: String,
    pub thread_id: Option<String>,
    pub notification_id: Option<i32>,
}

impl NativeNotification {
    pub fn message(
        title: impl Into<String>,
        body: impl Into<String>,
        thread_id: impl Into<String>,
        notification_id: i32,
    ) -> Self {
        Self {
            kind: NativeNotificationKind::Message,
            title: title.into(),
            body: body.into(),
            thread_id: Some(thread_id.into()),
            notification_id: Some(notification_id),
        }
    }

    pub fn game(
        title: impl Into<String>,
        body: impl Into<String>,
        thread_id: impl Into<String>,
        notification_id: i32,
    ) -> Self {
        Self {
            kind: NativeNotificationKind::Game,
            title: title.into(),
            body: body.into(),
            thread_id: Some(thread_id.into()),
            notification_id: Some(notification_id),
        }
    }

    pub fn call(
        title: impl Into<String>,
        body: impl Into<String>,
        thread_id: impl Into<String>,
        notification_id: i32,
    ) -> Self {
        Self {
            kind: NativeNotificationKind::Call,
            title: title.into(),
            body: body.into(),
            thread_id: Some(thread_id.into()),
            notification_id: Some(notification_id),
        }
    }
}

pub trait NativeNotifier: Send + Sync {
    fn notify(&self, notification: NativeNotification);
}

pub struct NoopNotifier;

impl NativeNotifier for NoopNotifier {
    fn notify(&self, _notification: NativeNotification) {}
}
