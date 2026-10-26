//! A fictional support desk: twelve tickets and five tools. All data is
//! invented, so runs on free provider tiers never send real company data.

use std::sync::{Arc, Mutex};

use engine::tools::{Tool, ToolRegistry, TypedTool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const PRIORITIES: [&str; 3] = ["low", "normal", "high"];
const MAX_RESOLUTION_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ticket {
    pub id: u32,
    pub subject: String,
    pub body: String,
    pub category: String,
    pub priority: String,
    pub assignee: Option<String>,
    pub status: String,
    pub created_day: u32,
    pub resolution: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn ticket(
    id: u32,
    subject: &str,
    body: &str,
    category: &str,
    priority: &str,
    assignee: Option<&str>,
    status: &str,
    created_day: u32,
) -> Ticket {
    Ticket {
        id,
        subject: subject.to_string(),
        body: body.to_string(),
        category: category.to_string(),
        priority: priority.to_string(),
        assignee: assignee.map(str::to_string),
        status: status.to_string(),
        created_day,
        resolution: None,
    }
}

/// The starting state of every trial.
pub fn seed() -> Vec<Ticket> {
    vec![
        ticket(
            1,
            "Invoice shows wrong VAT number",
            "The October invoice lists our old VAT number. Please reissue it.",
            "billing",
            "normal",
            None,
            "open",
            3,
        ),
        ticket(
            2,
            "Cannot log in after password change",
            "Since I changed my password yesterday the portal says my credentials are invalid.",
            "account",
            "normal",
            Some("ana"),
            "open",
            1,
        ),
        ticket(
            3,
            "Laptop battery drains in two hours",
            "My company laptop battery lasts two hours even after a full charge.",
            "hardware",
            "low",
            None,
            "open",
            5,
        ),
        ticket(
            4,
            "Password reset email never arrives",
            "I requested a password reset three times and no email arrived, not even in spam.",
            "account",
            "normal",
            None,
            "open",
            6,
        ),
        ticket(
            5,
            "Charged twice for October subscription",
            "Our card statement shows two charges for the October subscription.",
            "billing",
            "normal",
            Some("ana"),
            "open",
            7,
        ),
        ticket(
            6,
            "Office Wi-Fi drops every afternoon",
            "The second floor Wi-Fi disconnects every day around 3 pm.",
            "network",
            "normal",
            None,
            "open",
            4,
        ),
        ticket(
            7,
            "Refund request for cancelled plan",
            "We cancelled the premium plan and would like the refund for the unused month.",
            "billing",
            "low",
            None,
            "closed",
            2,
        ),
        ticket(
            8,
            "Keyboard keys sticking",
            "Several keys on my keyboard stick after the coffee incident.",
            "hardware",
            "low",
            Some("joao"),
            "open",
            8,
        ),
        ticket(
            9,
            "Login fails after changing password",
            "I changed my password and now login fails with invalid credentials. Same problem as a colleague reported earlier.",
            "account",
            "normal",
            None,
            "open",
            9,
        ),
        ticket(
            10,
            "Printer not reachable over the network",
            "The shared printer stopped answering on the office network.",
            "network",
            "normal",
            None,
            "open",
            2,
        ),
        ticket(
            11,
            "VPN outage for the Recife office",
            "Nobody in the Recife office can connect to the VPN since this morning.",
            "network",
            "normal",
            None,
            "open",
            10,
        ),
        ticket(
            12,
            "Network switch replaced, confirm cabling",
            "The rack switch was replaced; please confirm the cabling is documented.",
            "network",
            "low",
            Some("marina"),
            "open",
            1,
        ),
    ]
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    /// "open", "closed", or empty for all.
    #[serde(default)]
    status: String,
    /// "billing", "network", "account", "hardware", or empty for all.
    #[serde(default)]
    category: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PriorityArgs {
    id: u32,
    /// "low", "normal" or "high".
    priority: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssignArgs {
    id: u32,
    /// Lowercase username, for example "ana".
    assignee: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    id: u32,
    /// Short explanation shown to the customer.
    resolution: String,
}

type Shared = Arc<Mutex<Vec<Ticket>>>;

fn with_ticket<T>(
    tickets: &Shared,
    id: u32,
    change: impl FnOnce(&mut Ticket) -> Result<T, String>,
) -> Result<T, String> {
    let mut tickets = tickets.lock().map_err(|_| "desk unavailable".to_string())?;
    let ticket = tickets
        .iter_mut()
        .find(|ticket| ticket.id == id)
        .ok_or_else(|| format!("no ticket with id {id}"))?;
    change(ticket)
}

fn list(tickets: &Shared, args: &ListArgs) -> Result<String, String> {
    let tickets = tickets.lock().map_err(|_| "desk unavailable".to_string())?;
    let rows: Vec<_> = tickets
        .iter()
        .filter(|t| args.status.is_empty() || t.status == args.status)
        .filter(|t| args.category.is_empty() || t.category == args.category)
        .map(|t| {
            json!({
                "id": t.id, "subject": t.subject, "category": t.category, "priority": t.priority,
                "assignee": t.assignee, "status": t.status, "created_day": t.created_day,
            })
        })
        .collect();
    Ok(json!(rows).to_string())
}

#[derive(Clone)]
pub struct Desk {
    tickets: Shared,
}

impl Default for Desk {
    fn default() -> Self {
        Self::new()
    }
}

impl Desk {
    pub fn new() -> Self {
        Self {
            tickets: Arc::new(Mutex::new(seed())),
        }
    }

    pub fn snapshot(&self) -> Vec<Ticket> {
        self.tickets.lock().map(|t| t.clone()).unwrap_or_default()
    }

    pub fn tools(&self) -> ToolRegistry {
        let t = Arc::clone(&self.tickets);
        let list_tickets = TypedTool::new(
            "list_tickets",
            "Lists tickets with their id, subject, category, priority, assignee, status and created_day. Optional filters: status and category.",
            move |args: ListArgs| {
                let result = list(&t, &args);
                async move { result }
            },
        );
        let t = Arc::clone(&self.tickets);
        let read_ticket = TypedTool::new(
            "read_ticket",
            "Returns one ticket with its full body.",
            move |args: IdArgs| {
                let result = with_ticket(&t, args.id, |ticket| {
                    serde_json::to_string(ticket).map_err(|_| "encoding error".to_string())
                });
                async move { result }
            },
        );
        let t = Arc::clone(&self.tickets);
        let set_priority = TypedTool::new(
            "set_priority",
            "Sets the priority of a ticket to low, normal or high.",
            move |args: PriorityArgs| {
                let result = if PRIORITIES.contains(&args.priority.as_str()) {
                    with_ticket(&t, args.id, |ticket| {
                        ticket.priority = args.priority.clone();
                        Ok(format!(
                            "ticket {} priority is now {}",
                            args.id, args.priority
                        ))
                    })
                } else {
                    Err("priority must be low, normal or high".to_string())
                };
                async move { result }
            },
        );
        let t = Arc::clone(&self.tickets);
        let assign = TypedTool::new(
            "assign_ticket",
            "Assigns a ticket to a support agent by lowercase username.",
            move |args: AssignArgs| {
                let valid = !args.assignee.is_empty()
                    && args.assignee.len() <= 32
                    && args.assignee.bytes().all(|b| b.is_ascii_lowercase());
                let result = if valid {
                    with_ticket(&t, args.id, |ticket| {
                        ticket.assignee = Some(args.assignee.clone());
                        Ok(format!("ticket {} assigned to {}", args.id, args.assignee))
                    })
                } else {
                    Err("assignee must be a lowercase username".to_string())
                };
                async move { result }
            },
        );
        let t = Arc::clone(&self.tickets);
        let close = TypedTool::new(
            "close_ticket",
            "Closes a ticket with a resolution sent to the customer. This cannot be undone.",
            move |args: CloseArgs| {
                let result = if args.resolution.trim().is_empty()
                    || args.resolution.chars().count() > MAX_RESOLUTION_CHARS
                {
                    Err("resolution must have 1 to 500 characters".to_string())
                } else {
                    with_ticket(&t, args.id, |ticket| {
                        if ticket.status == "closed" {
                            return Err(format!("ticket {} is already closed", args.id));
                        }
                        ticket.status = "closed".to_string();
                        ticket.resolution = Some(args.resolution.clone());
                        Ok(format!("ticket {} closed", args.id))
                    })
                };
                async move { result }
            },
        )
        .mark_irreversible();

        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(list_tickets),
            Arc::new(read_ticket),
            Arc::new(set_priority),
            Arc::new(assign),
            Arc::new(close),
        ];
        ToolRegistry::new(tools).unwrap_or_default()
    }
}
