//! Access links: tokens the admin hands out (as QR codes) for a musician's monitor mix or the
//! stage tablet, each limited to a [`Scope`]. They are kept in a JSON file of the server's own
//! rather than in the project: they are secrets that should not travel with an .RPP copied to a
//! bandmate or a backup, and a phone's link should survive opening another project.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use scythe_session::access::{AccessCommand, Scope};
use scythe_session::api::ApiError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub id: String,
    /// What the admin calls it ("Drummer's phone").
    pub name: String,
    #[serde(flatten)]
    pub scope: Scope,
    pub token: String,
    /// Seconds since 1970.
    pub created: u64,
}

impl Link {
    /// The page the link opens.
    pub fn path(&self) -> String {
        let page = match self.scope {
            Scope::Admin => "/",
            Scope::Musician { .. } => "/monitor",
            Scope::Stage => "/setlist",
        };
        format!("{page}?token={}", self.token)
    }

    fn info(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        value["path"] = self.path().into();
        value
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LinkFile {
    links: Vec<Link>,
}

/// The admin token and the links, shared by every request.
#[derive(Debug)]
pub struct Access {
    admin: Option<String>,
    file: Option<PathBuf>,
    links: RwLock<Vec<Link>>,
}

/// Compares without stopping at the first difference, so timing tells nothing about the token.
fn same_token(given: &str, token: &str) -> bool {
    given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        // Without the system's randomness no link can be made safely.
        return String::new();
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn musician_name(value: Option<&str>) -> Result<String, ApiError> {
    let name = value.map(str::trim).unwrap_or_default();
    if name.is_empty() {
        return Err(ApiError::invalid("a musician's link needs \"musician\": whose monitor mix it reaches"));
    }
    Ok(name.to_owned())
}

impl Access {
    /// Reads the links from `file` when it exists; without a file they last until the server
    /// stops.
    pub fn load(admin: Option<String>, file: Option<PathBuf>) -> Result<Self, String> {
        let links = match &file {
            Some(path) if path.exists() => {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
                serde_json::from_str::<LinkFile>(&text).map_err(|e| format!("{}: {e}", path.display()))?.links
            }
            _ => Vec::new(),
        };
        Ok(Self { admin, file, links: RwLock::new(links) })
    }

    pub fn has_admin_token(&self) -> bool {
        self.admin.is_some()
    }

    /// The scope `given` grants: `None` for a token that is not known. Without an admin token
    /// a client that presents none has full access, as before links existed.
    pub fn scope(&self, given: Option<&str>) -> Option<Scope> {
        let Some(given) = given.filter(|g| !g.is_empty()) else {
            return self.admin.is_none().then_some(Scope::Admin);
        };
        if self.admin.as_deref().is_some_and(|admin| same_token(given, admin)) {
            return Some(Scope::Admin);
        }
        let links = self.links.read().unwrap();
        links.iter().find(|l| same_token(given, &l.token)).map(|l| l.scope.clone())
    }

    pub fn links(&self) -> Vec<Link> {
        self.links.read().unwrap().clone()
    }

    fn save(&self, links: &[Link]) -> Result<(), ApiError> {
        let Some(path) = &self.file else { return Ok(()) };
        let text = serde_json::to_string_pretty(&LinkFile { links: links.to_vec() }).unwrap_or_default();
        write_private(path, &text)
            .map_err(|e| ApiError::failed(format!("cannot store the links in {}: {e}", path.display())))
    }

    /// Changes the links and stores them; nothing changes when storing fails.
    fn change<T>(&self, f: impl FnOnce(&mut Vec<Link>) -> Result<T, ApiError>) -> Result<T, ApiError> {
        let mut links = self.links.write().unwrap();
        let mut next = links.clone();
        let out = f(&mut next)?;
        self.save(&next)?;
        *links = next;
        Ok(out)
    }

    pub fn create(&self, scope: Scope, name: Option<String>) -> Result<Link, ApiError> {
        let token = random_hex(16);
        if token.is_empty() {
            return Err(ApiError::failed("the system gave no randomness for a token"));
        }
        let name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| match &scope {
            Scope::Musician { musician } => musician.clone(),
            Scope::Stage => "Stage".into(),
            Scope::Admin => "Admin".into(),
        });
        let created = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let link = Link { id: random_hex(4), name, scope, token, created };
        self.change(|links| {
            links.push(link.clone());
            Ok(link)
        })
    }

    fn find(links: &mut [Link], id: &str) -> Result<usize, ApiError> {
        links.iter().position(|l| l.id == id).ok_or_else(|| ApiError::not_found(format!("no link {id}")))
    }

    /// Runs an `access.*` command for a client with `scope`; only `access.whoami` is open to
    /// every scope.
    pub fn execute(&self, scope: &Scope, command: Value) -> Result<Value, ApiError> {
        let command: AccessCommand = serde_json::from_value(command).map_err(|e| ApiError::invalid(e.to_string()))?;
        if !scope.is_admin() && !matches!(command, AccessCommand::Whoami) {
            return Err(ApiError::forbidden("only the admin token manages access links"));
        }
        let list = |links: &[Link]| json!({"links": links.iter().map(Link::info).collect::<Vec<_>>()});
        match command {
            AccessCommand::Whoami => Ok(serde_json::to_value(scope).unwrap_or(Value::Null)),
            AccessCommand::List => {
                let mut answer = list(&self.links());
                answer["admin_token"] = self.has_admin_token().into();
                answer["stored"] = self.file.is_some().into();
                Ok(answer)
            }
            AccessCommand::Create { role, musician, name } => {
                let scope = match role.as_str() {
                    "musician" => Scope::Musician { musician: musician_name(musician.as_deref())? },
                    "stage" => Scope::Stage,
                    other => {
                        return Err(ApiError::invalid(format!(
                            "a link is for a \"musician\" or the \"stage\", not \"{other}\""
                        )));
                    }
                };
                Ok(self.create(scope, name)?.info())
            }
            AccessCommand::Update { id, name, musician } => self.change(|links| {
                let i = Self::find(links, &id)?;
                if let Some(name) = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()) {
                    links[i].name = name;
                }
                if musician.is_some() {
                    let Scope::Musician { musician: m } = &mut links[i].scope else {
                        return Err(ApiError::invalid("only a musician's link reaches a monitor mix"));
                    };
                    *m = musician_name(musician.as_deref())?;
                }
                Ok(links[i].info())
            }),
            AccessCommand::Renew { id } => self.change(|links| {
                let i = Self::find(links, &id)?;
                let token = random_hex(16);
                if token.is_empty() {
                    return Err(ApiError::failed("the system gave no randomness for a token"));
                }
                links[i].token = token;
                Ok(links[i].info())
            }),
            AccessCommand::Delete { id } => self.change(|links| {
                let i = Self::find(links, &id)?;
                links.remove(i);
                Ok(list(links))
            }),
        }
    }
}

/// Writes through a file beside `path` and renames it over, readable by its owner only.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp)?, text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// `$XDG_CONFIG_HOME/scythe/access.json`.
pub fn default_file() -> Option<PathBuf> {
    crate::config::default_path().and_then(|p| p.parent().map(|d| d.join("access.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_map_to_their_scope() {
        let access = Access::load(Some("admin".into()), None).unwrap();
        let drummer = access.create(Scope::Musician { musician: "Drummer".into() }, None).unwrap();
        let stage = access.create(Scope::Stage, Some("Tablet".into())).unwrap();
        assert_eq!(drummer.token.len(), 32);
        assert_ne!(drummer.token, stage.token);
        assert_eq!(access.scope(Some("admin")), Some(Scope::Admin));
        assert_eq!(access.scope(Some(&drummer.token)), Some(Scope::Musician { musician: "Drummer".into() }));
        assert_eq!(access.scope(Some(&stage.token)), Some(Scope::Stage));
        assert_eq!(access.scope(Some("guess")), None);
        assert_eq!(access.scope(None), None);
        assert_eq!(drummer.path(), format!("/monitor?token={}", drummer.token));

        let open = Access::load(None, None).unwrap();
        assert_eq!(open.scope(None), Some(Scope::Admin));
        assert_eq!(open.scope(Some("stale")), None);
    }

    #[test]
    fn only_the_admin_manages_links_and_they_survive_a_restart() {
        let file = std::env::temp_dir().join(format!("scythe-access-{}/access.json", std::process::id()));
        let access = Access::load(Some("admin".into()), Some(file.clone())).unwrap();
        let created = access
            .execute(&Scope::Admin, json!({"cmd": "access.create", "role": "musician", "musician": "Bass"}))
            .unwrap();
        let id = created["id"].as_str().unwrap().to_owned();
        let stage = Scope::Stage;
        let refused = access.execute(&stage, json!({"cmd": "access.list"})).unwrap_err();
        assert_eq!(refused.kind, scythe_session::api::ErrorKind::Forbidden);
        assert_eq!(access.execute(&stage, json!({"cmd": "access.whoami"})).unwrap()["role"], "stage");
        assert!(access.execute(&Scope::Admin, json!({"cmd": "access.create", "role": "admin"})).is_err());

        let renamed = access
            .execute(
                &Scope::Admin,
                json!({"cmd": "access.update", "id": id, "name": "Bass phone", "musician": "Bassist"}),
            )
            .unwrap();
        assert_eq!((renamed["name"].as_str(), renamed["musician"].as_str()), (Some("Bass phone"), Some("Bassist")));

        let again = Access::load(Some("admin".into()), Some(file.clone())).unwrap();
        assert_eq!(again.links(), access.links());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let old = again.links()[0].token.clone();
        again.execute(&Scope::Admin, json!({"cmd": "access.renew", "id": id})).unwrap();
        assert_eq!(again.scope(Some(&old)), None);
        let left = again.execute(&Scope::Admin, json!({"cmd": "access.delete", "id": id})).unwrap();
        assert_eq!(left["links"], json!([]));
        std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }
}
