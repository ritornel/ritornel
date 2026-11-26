//! About: this app, the server it is connected to, and the contract the two
//! speak.

use gpui::{App, InteractiveElement, IntoElement, ParentElement, Styled, div};
use ritornel_contract::{CONTRACT_VERSION, ServerRole};

use super::controls::{actions, boxed, group, grow, heading, label_value, push, row, sub};
use super::model::Model;

/// What a role means, in a line.
fn role(role: ServerRole) -> &'static str {
    match role {
        ServerRole::Hub => "Hub: it holds the library",
        ServerRole::Satellite => "Linked to a hub",
        ServerRole::Unassigned => "No library yet",
    }
}

pub(super) fn render(model: &Model, cx: &App) -> impl IntoElement {
    let server = model.server(cx);
    let status = model.status.as_ref();
    let version = status.map_or_else(
        || "—".to_owned(),
        |status| {
            let contract = status.versions.contract;
            if contract == CONTRACT_VERSION {
                status.versions.server.clone()
            } else {
                format!("{}, contract {contract}", status.versions.server)
            }
        },
    );
    let reveal = model.data_dir.clone();
    div()
        .id("about")
        .flex()
        .flex_col()
        .child(group(true).child(heading("Ritornel")).child(boxed(vec![
            label_value("Version", env!("RITORNEL_VERSION_DISPLAY")),
            label_value("Contract", CONTRACT_VERSION.to_string()),
        ])))
        .child(group(false).child(heading("Server")).child(boxed(vec![
            label_value(
                "Name",
                server
                    .as_ref()
                    .map_or_else(|| "—".to_owned(), |net| net.name.clone()),
            ),
            label_value(
                "Role",
                server.as_ref().map_or("—", |net| role(net.role)),
            ),
            label_value("Version", version),
            row()
                .child(grow().child("Data folder").child(sub(
                    model.data_dir.as_ref().map_or_else(
                        || "Could not be worked out".to_owned(),
                        |dir| dir.display().to_string(),
                    ),
                )))
                .child(actions().child(push(
                    "show-data",
                    crate::platform::SHOW_FOLDER,
                    reveal.is_some(),
                    move |_, _, cx| {
                        if let Some(dir) = &reveal {
                            cx.reveal_path(dir);
                        }
                    },
                ))),
        ])))
}
