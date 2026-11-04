use super::*;

pub(super) fn optional<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(s)) if s.len() <= max && !s.chars().any(char::is_control) => Ok(s),
        _ => Err(INVALID.into()),
    }
}
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}
impl Catalog<'_> {
    fn project_name(&self, id: &str) -> String {
        if let Some(p) = self.projects.get(id) {
            let name = p["name"].as_str().unwrap_or_default();
            if !name.is_empty() {
                return if self.projects.values().filter(|p| p["name"] == name).count() > 1 {
                    format!("{name} ({})", short(id))
                } else {
                    name.into()
                };
            }
        }
        if id == super::super::INBOX {
            "General".into()
        } else {
            format!("Unavailable project ({})", short(id))
        }
    }
    fn project(&self, id: &str) -> Option<Project> {
        self.projects.get(id).map(|p| Project {
            id: id.into(),
            name: self.project_name(id),
            color: p["color"].as_str().map(str::to_owned),
        })
    }
    fn machine(&self, id: &str, origin: bool) -> Origin {
        let machine = self.machines.get(id);
        let local = machine.is_some_and(|m| m["local"] == true) || self.snapshot["node_id"] == id;
        let name = machine.and_then(|m| {
            m["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| m["connection"].as_str().filter(|s| !s.is_empty()))
        });
        let mut computer = machine
            .and_then(|m| m["connection"].as_str().filter(|s| !s.is_empty()))
            .map(str::to_owned);
        if local && !self.local_host.is_empty() {
            computer = Some(self.local_host.into());
        }
        let name =
            name.or_else(|| (local && !self.local_host.is_empty()).then_some(self.local_host));
        let text = match name {
            Some(s) if origin && local => format!("{s} (this computer)"),
            Some(s) => s.into(),
            None if local => "This computer".into(),
            None => format!("Unknown computer ({})", short(id)),
        };
        Origin {
            text,
            computer,
            local: origin && local,
        }
    }
    pub(super) fn summary(&self, raw: &Value) -> Result<Summary, String> {
        let field = Field::checked(&raw["field"])?;
        let (subject, project, computer) = match &field {
            Field::Project { id, .. } => (self.project_name(id), self.project(id), None),
            Field::Machine { id, .. } => {
                let m = self.machine(id, false);
                (m.text, None, m.computer)
            }
            Field::Folder { id } => (
                self.folders
                    .get(id.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        variants(raw).iter().find_map(|op| {
                            op["value"]["path"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                        })
                    })
                    .unwrap_or_else(|| format!("Folder link ({})", short(id))),
                None,
                None,
            ),
            Field::Session { machine, session } => {
                let mut title = session
                    .strip_prefix("archive\n")
                    .map(|id| format!("Archived session ({})", short(id)))
                    .unwrap_or_else(|| session.clone());
                if let Some(found) = self
                    .session_title
                    .and_then(|lookup| lookup(machine, session))
                {
                    if found.len() > 2048 || found.chars().any(char::is_control) {
                        return Err(INVALID.into());
                    }
                    title = if session.starts_with("archive\n") {
                        format!("Archived session: {found}")
                    } else {
                        found
                    };
                }
                let m = self.machine(machine, false);
                (format!("{title} on {}", m.text), None, m.computer)
            }
        };
        Ok(Summary {
            id: token_id(raw),
            setting: field.setting().into(),
            subject,
            project,
            computer,
        })
    }
}
impl Field {
    fn setting(&self) -> &'static str {
        match self {
            Self::Project { field, .. } => match field.as_str() {
                "name" => "Project name",
                "color" => "Project color",
                _ => "Keep or remove project",
            },
            Self::Folder { .. } => "Project folder",
            Self::Session { .. } => "Session project",
            Self::Machine { field, .. } if field == "name" => "Computer name",
            _ => "Computer identity",
        }
    }
}
impl Review {
    fn value_text(&self, c: &Catalog<'_>, value: &Value) -> String {
        match &self.field {
            Field::Session { .. } => {
                if value.is_null() {
                    "Use the local default project".into()
                } else {
                    c.project_name(value.as_str().unwrap_or_default())
                }
            }
            Field::Folder { .. } if value.is_null() => "Remove folder link from the catalog".into(),
            Field::Folder { .. } => {
                let name = value["name"].as_str().unwrap_or_default();
                format!(
                    "{}Project: {}\nComputer: {}\n{}",
                    if name.is_empty() {
                        String::new()
                    } else {
                        format!("Name: {name}\n")
                    },
                    c.project_name(value["project"].as_str().unwrap_or_default()),
                    c.machine(value["machine"].as_str().unwrap_or_default(), false)
                        .text,
                    value["path"].as_str().unwrap_or_default()
                )
            }
            Field::Project { field, .. } if field == "alive" => if value == true {
                "Keep project in the catalog"
            } else {
                "Remove project from the catalog"
            }
            .into(),
            Field::Machine { field, .. } if field == "redirect" => {
                c.machine(value.as_str().unwrap_or_default(), false).text
            }
            _ => value.as_str().unwrap_or_default().into(),
        }
    }
    pub(super) fn choice(&self, c: &Catalog<'_>, index: usize) -> Result<Choice, String> {
        let value = self.value(index)?;
        let project = match &self.field {
            Field::Session { .. } if value.is_string() => c.project(value.as_str().unwrap()),
            Field::Folder { .. } if value.is_object() => {
                c.project(value["project"].as_str().unwrap_or_default())
            }
            Field::Project { id, field } if matches!(field.as_str(), "name" | "color") => {
                c.project(id).map(|mut p| {
                    if field == "name" {
                        p.name = value.as_str().unwrap().into();
                    } else {
                        p.color = value.as_str().map(str::to_owned);
                    }
                    p
                })
            }
            _ => None,
        };
        let mut origins = Vec::new();
        let mut shown = false;
        for i in &self.groups[index] {
            let op = &variants(&self.raw)[*i];
            let m = c.machine(op["actor"].as_str().unwrap(), true);
            if !origins.iter().any(|p: &Origin| p.text == m.text) {
                origins.push(m);
            }
            shown |= op["id"] == self.raw["selected"];
        }
        Ok(Choice {
            index,
            text: self.value_text(c, value),
            project,
            origins,
            shown,
            removal: self.field.removal(value),
        })
    }
    pub(super) fn consequence(&self, c: &Catalog<'_>, value: &Value) -> String {
        let mut result = match &self.field {
            Field::Session { .. } => {
                let text = if value.is_null() {
                    "Remove the explicit project assignment. Each computer will use its local default project.".into()
                } else {
                    format!("Assign this session to “{}”.", self.value_text(c, value))
                };
                format!(
                    "{text} The session, its messages and running agent stay intact.{}",
                    if value
                        .as_str()
                        .is_some_and(|id| !c.projects.contains_key(id))
                    {
                        "\nThis project is not in the current catalog. Until it is restored, the session is displayed under each computer's local default project."
                    } else {
                        ""
                    }
                )
            }
            Field::Folder { .. } => format!(
                "{}\nFiles on disk stay intact.",
                if value.is_null() {
                    "Remove this folder link from the shared catalog.".into()
                } else {
                    format!("Keep this folder location:\n{}", self.value_text(c, value))
                }
            ),
            Field::Project { id, field } if field == "alive" => {
                if value == true {
                    format!("Keep “{}” in the shared catalog.", c.project_name(id))
                } else {
                    format!("Remove “{}” from the shared catalog. Its folder links and project placement will no longer be displayed. Files, sessions and messages stay intact.", c.project_name(id))
                }
            }
            _ => format!(
                "Set {} to “{}”.",
                self.field.setting().to_lowercase(),
                self.value_text(c, value)
            ),
        };
        result.push_str("\nThis choice will synchronize to connected computers. Offline computers receive it when they reconnect.");
        result
    }
}
