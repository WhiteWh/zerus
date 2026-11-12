//! Mirror Claude's native tool approval panels in Activity. A pending
//! main-agent or child PermissionRequest stays visible while Claude shows a chooser. A
//! complete panel that matches the request exposes its native options verbatim;
//! options reaching beyond this one request are marked for a second
//! confirmation in the desktop. The separate explicit desktop automatic-approval
//! policy may choose only a reviewed one-time grant, with a durable audit record. An
//! answer given in Terminal sends no hook; only the screen's proof settles it.
//! While Claude's permission hook waits for a main-agent request (`channel`),
//! a one-time decision goes to that exact tool use through the hook instead of
//! the keys, so a panel taller than its pane can still be answered.
use super::*;
use sha2::{Digest,Sha256};
use std::path::Path;
pub(super) mod channel;
mod children;
pub(super) fn observe(record:&mut Value,event:&Value) {children::observe(record,event);channel::observe(record,event);}

#[derive(Clone,Debug,PartialEq)]
struct Panel { title:String, body:Vec<String>, question:String, labels:Vec<String>, selected:usize }

// Claude also reports AskUserQuestion as a PermissionRequest; claude_question
// owns that card, or native_question when no tool call exists. Plan approval can turn an option into a text field, so its
// card only points to Terminal.
const QUESTION_TOOLS:[&str;3]=["AskUserQuestion","request_user_input","AskUser"];
const TERMINAL_ONLY_TOOLS:[&str;2]=["ExitPlanMode","EnterPlanMode"];
// One-time answers. Every other label, including an unknown one, grants,
// blocks or switches more than the visible request.
const ONCE:[&str;6]=["Yes","No","Yes, but ask again next time","No, and ask again next time",
    "No, and tell Claude what to do differently (esc)","No, and tell Claude what to do differently"];

pub(super) fn question_tool(tool:&str)->bool {QUESTION_TOOLS.contains(&tool)}
fn rule(row:&str,c:char)->bool {row.chars().count()>=20&&row.chars().all(|x|x==c)}
fn numbered(row:&str)->Option<(bool,usize,String)> {
    let (mark,rest)=match row.strip_prefix('❯'){Some(rest)=>(true,rest.trim_start()),None=>(false,row)};
    let (number,label)=rest.split_once(". ")?;
    if number.is_empty()||number.len()>2||!number.chars().all(|c|c.is_ascii_digit())||label.trim().is_empty(){return None;}
    Some((mark,number.parse().ok()?,label.trim().to_owned()))
}
fn label(text:&str)->String {text.split_whitespace().collect::<Vec<_>>().join(" ")}
fn compact(text:&str)->String {text.chars().filter(|c|!c.is_whitespace()).collect()}

fn gutter(row:&str)->&str {row.strip_prefix("│ ").unwrap_or(row)}

// This advisory counts down, then disappears when the user starts navigating.
// It is not the command's warning or a permission choice. Preserve display
// text, but exclude only these exact native advisories from the decision identity.
fn identity_body(panel:&Panel)->Vec<String> {
    static TIMER:std::sync::OnceLock<regex::Regex>=std::sync::OnceLock::new();
    let timer=TIMER.get_or_init(||regex::Regex::new(r"^⚠ Claude Code will automatically (?:deny this request in [0-9]{1,3}:[0-9]{2}, to avoid blocking progress on an unattended session|block this request in [0-9]{1,3}:[0-9]{2}, to avoid holding up an unattended session)$").unwrap());
    let after_fence=panel.body.iter().rposition(|r|rule(r,'╌')||rule(r,'─'))
        .map_or(if panel.title=="Bash safety confirmation"{0}else{panel.body.len()},|i|i+1);
    let mut result=Vec::new();let mut index=0;
    while index<panel.body.len() {
        if index>=after_fence&&panel.body[index].starts_with("⚠ Claude Code ") {
            let mut advisory=String::new();let mut end=index;
            while end<panel.body.len()&&end<index+8 {
                if !advisory.is_empty(){advisory.push(' ');}
                advisory.push_str(&panel.body[end]);end+=1;
                let text=advisory.split_whitespace().collect::<Vec<_>>().join(" ");
                if timer.is_match(&text) {
                    index=end;break;
                }
            }
            if index==end {continue;}
        }
        result.push(panel.body[index].clone());index+=1;
    }
    while result.last().is_some_and(String::is_empty){result.pop();}
    result
}

/// Any highlighted numbered choice below Claude's last full-width rule. The
/// idle composer sits between two rules, so a resolved request has none.
fn visible(screen:&str)->bool {
    let rows:Vec<_>=screen.lines().map(str::trim).collect();
    let start=rows.iter().rposition(|r|rule(r,'─')).map_or(0,|i|i+1);
    rows[start..].iter().any(|r|numbered(r).is_some_and(|(mark,_,_)|mark))
}

/// The chooser at the bottom of the screen: a question, its consecutive
/// numbered options with exactly one highlighted, wrapped option rows, then
/// only blank rows and an optional separated "Esc to ..." footer.
fn chooser(rows:&[&str])->Option<(usize,Vec<String>,usize)> {
    let mut end=rows.len();
    while end>0&&rows[end-1].is_empty(){end-=1;}
    // Ink wraps the footer like any other text, so it is the last block.
    let block=rows[..end].iter().rposition(|r|r.is_empty()).map_or(0,|i|i+1);
    if let Some(footer)=(block..end).find(|&i|rows[i].starts_with("Esc to ")) {
        if footer!=block||block==0 {return None;}
        end=block;
        while end>0&&rows[end-1].is_empty(){end-=1;}
    }
    let first=(0..end).rev().find(|&i|numbered(rows[i]).is_some_and(|(_,n,_)|n==1))?;
    let question=first.checked_sub(1)?;
    if !rows[question].ends_with('?')||numbered(rows[question]).is_some(){return None;}
    let mut labels:Vec<String>=Vec::new();let mut selected=None;
    for row in &rows[first..end] {
        match numbered(row) {
            Some((mark,n,text)) if n==labels.len()+1 => {
                if mark&&selected.replace(labels.len()).is_some(){return None;}
                labels.push(text);
            }
            Some(_) => return None,
            None if row.is_empty()||row.contains('❯') => return None,
            None => labels.last_mut()?.push_str(&format!("\n{row}")),
        }
    }
    if labels.len()<2||labels.len()>9 {return None;}
    Some((question,labels,selected?))
}

/// A chooser whose panel begins above the top row. Claude keeps no scrollback
/// for its panel, so the title and the start of the request show nowhere.
fn clipped(screen:&str)->bool {
    let rows:Vec<_>=screen.lines().map(str::trim).collect();
    visible(screen)&&chooser(&rows).is_some_and(|(question,_,_)|!rows[..question].iter().any(|r|rule(r,'─')))
}
fn complete_panel(screen:&str)->bool {parse(screen).is_some()}
/// Only a recorded main or child request may resize its pane to show the panel.
fn fits(record:&Value)->bool {request(record).is_some()||children::entries(record).next().is_some()}

fn parse(screen:&str)->Option<Panel> {
    let rows:Vec<_>=screen.lines().map(str::trim).collect();
    let (question,labels,selected)=chooser(&rows)?;
    // A panel taller than the terminal loses its top rule and title.
    let top=rows[..question].iter().rposition(|r|rule(r,'─'))?;
    let title=*rows.get(top+1).filter(|r|top+1<question&&!r.is_empty()&&!rule(r,'╌'))?;
    let mut body:Vec<String>=rows[top+2..question].iter().map(|r|gutter(r).to_string()).collect();
    while body.last().is_some_and(String::is_empty){body.pop();}
    Some(Panel{title:title.into(),body,question:rows[question].into(),labels,selected})
}

fn active_phase(record:&Value)->bool {
    ["working","tool","input","approval","unknown"].contains(&string(record,"phase"))
        ||(record["phase"]=="idle"&&children::entries(record).next().is_some())
}

fn eligible(record:&Value)->bool {
    record["agent"]=="claude"&&record["phase"]=="approval"&&!pause_active(record)&&string(record,"error").is_empty()
}
fn request(record:&Value)->Option<&Value> {
    let request=&record["pending_approval"];
    let tool=string(request,"tool_name");
    (request.is_object()&&!tool.is_empty()&&!question_tool(tool)).then_some(request)
}

/// The panel must show the request the agent made. Bash keeps its exact
/// command check; file and fetch panels must name their target.
fn matches(request:&Value,panel:&Panel)->bool {
    let input=&request["tool_input"];
    if request["tool_name"]=="Bash" {
        let command=string(input,"command");
        if command.trim().is_empty()||command.len()>64000 {return false;}
        let dividers:Vec<_>=panel.body.iter().enumerate().filter(|(_,r)|rule(r,'╌')).map(|(i,_)|i).collect();
        // Rows are wrapped to the terminal: a long path continues on the
        // next row without a space, so compare across row boundaries.
        return match dividers[..] {
            [first,second] => question_terminal::wrapped(&panel.body[first+1..second],command),
            // Claude 2.1.284 renders the command and its description without
            // code fences. Match the entire body, including the optional tip.
            [] => {
                const TIP:&str="Tip: auto mode handles these prompts for you — choose \"switch to auto mode\" below";
                let content=format!("{command}\n{}",string(input,"description"));
                question_terminal::wrapped(&panel.body,&content)||question_terminal::wrapped(&panel.body,&format!("{TIP}\n{content}"))
            }
            _ => false,
        };
    }
    let text=compact(&format!("{}\n{}\n{}",panel.title,panel.body.join("\n"),panel.question));
    let mut targets=Vec::new();
    for key in ["file_path","notebook_path","path"] {
        if let Some(path)=input[key].as_str().filter(|p|!p.is_empty()) {
            targets.push(Path::new(path).file_name().and_then(|n|n.to_str()).unwrap_or(path).to_owned());
        }
    }
    if let Some(url)=input["url"].as_str().filter(|u|!u.is_empty()) {
        targets.push(url.split("://").nth(1).unwrap_or(url).split(['/','?','#']).next().unwrap_or(url).to_owned());
    }
    targets.iter().all(|target|text.contains(&compact(target)))
}

fn summary(request:&Value)->String {
    let input=&request["tool_input"];
    let target=["command","file_path","notebook_path","url","path","pattern"].iter()
        .find_map(|key|input[*key].as_str().filter(|v|!v.trim().is_empty()))
        .map(str::to_owned)
        .unwrap_or_else(||journal::clipped(&json!(input.to_string()),2000));
    format!("{}\n\n{}",string(request,"tool_name"),target)
}

fn tool_call_id(record:&Value,tool:&str)->String {
    let ids:Vec<_>=record["active_tools"].as_object().into_iter().flatten().filter(|(_,v)|v["name"]==tool).map(|(k,_)|k.clone()).collect();
    if ids.len()==1 {ids[0].clone()} else {String::new()}
}

/// The card for the pending request, if Claude still shows a chooser. Without
/// a terminal snapshot the request stays visible but cannot be answered here.
fn build(record:&Value,screen:Option<&str>)->Option<(Value,Option<Panel>)> {
    if record["agent"]!="claude"||pause_active(record)||!string(record,"error").is_empty()
        ||!active_phase(record)
        ||record.get("exited_at").is_some()||record.get("session_end").is_some() {return None;}
    let panel=screen.and_then(parse);
    // Include unsupported child tools in the ambiguity check: a matching
    // filename alone must not let a parent approval consume a child's panel.
    let matching:Vec<_>=children::entries(record).filter(|(_,entry)|
        panel.as_ref().is_some_and(|p|matches(&entry["request"],p))).collect();
    let main_matches=request(record).is_some_and(|r|panel.as_ref().is_some_and(|p|matches(r,p)));
    if children::overflow(record)||matching.len()+usize::from(main_matches)>1 {
        let (mut card,panel)=if let Some((id,entry))=matching.first() {
            build_request(record,screen,&entry["request"],&entry["at"],id)?
        } else if let Some(request)=request(record) {
            build_request(record,screen,request,&record["pending_approval_at"],"")?
        } else {return safety_card(record,screen?).map(|card|(card,panel));};
        card["question_hash"]=json!(format!("ambiguous:{}",string(&card,"question_hash")));
        card["question_id"]=json!(format!("claude-approval:{}",string(&card,"question_hash")));
        card["can_answer"]=json!(false);card["approval_choices"]=json!(false);
        card["answer_unavailable_reason"]=json!("The visible approval cannot be bound to one native request. Answer it in Terminal.");
        return Some((card,panel));
    }
    if let [(id,entry)]=matching[..] {
        // Only Bash currently proves the full child tool input. File names
        // and hosts alone are insufficient to identify a child's operation.
        if entry["request"]["tool_name"]=="Bash"&&panel.as_ref().is_some_and(|p|bash_title(&p.title)) {
            return build_request(record,screen,&entry["request"],&entry["at"],id);
        }
        let (mut card,panel)=build_request(record,screen,&entry["request"],&entry["at"],id)?;
        card["can_answer"]=json!(false);card["approval_choices"]=json!(false);
        card["answer_unavailable_reason"]=json!("This child tool request needs review in Terminal.");
        return Some((card,panel));
    }
    if !eligible(record) {return None;}
    build_request(record,screen,request(record)?,&record["pending_approval_at"],"")
}

fn build_request(record:&Value,screen:Option<&str>,request:&Value,at:&Value,child_request_id:&str)->Option<(Value,Option<Panel>)> {
    if screen.is_some_and(|screen|!visible(screen)) {return None;}
    let tool=string(request,"tool_name");
    let panel=screen.and_then(parse);
    let problem=match &panel {
        None if screen.is_none() => "Zerus cannot read this terminal. Answer the request in Terminal.",
        None => "Zerus cannot read this approval completely. Enlarge Terminal or answer it there.",
        Some(_) if TERMINAL_ONLY_TOOLS.contains(&tool) => "Review this plan in Terminal.",
        Some(panel) if !matches(request,panel) => "The visible approval does not match the agent's request. Answer it in Terminal.",
        Some(_) => "",
    };
    let native=panel.as_ref().filter(|_|problem.is_empty());
    let hash=format!("{:x}",Sha256::digest(json!([record["run_id"],record["conversation_id"],record["pane"],record["pid"],
        record["process_start"],tool,request["tool_input"],request["agent_id"],child_request_id,at,native.map(|p|json!([p.title,identity_body(p),p.question,p.labels]))]).to_string()));
    let (question,body,options)=match native {
        Some(panel) => (panel.question.clone(),native_body(panel),native_options(panel)),
        None => (format!("Claude asks to use {tool}"),with_warning(summary(request),screen),Vec::new()),
    };
    let created=at.as_f64().map_or(record["last_event_at"].clone(),|at|json!(at));
    let tool_id=if child_request_id.is_empty(){tool_call_id(record,tool)}else{string(request,"tool_use_id").to_owned()};
    Some((json!({"question_id":format!("claude-approval:{hash}"),"question_hash":hash,"tool_call_id":tool_id,"agent_id":request["agent_id"],"child_request_id":child_request_id,
        "run_id":record["run_id"],"conversation_id":record["conversation_id"],"source":"claude_tool_approval",
        "can_answer":native.is_some(),"answer_unavailable_reason":problem,"approval":true,"approval_choices":native.is_some(),
        "answer_transport":"claude_tui","created_at":created,
        "questions":[{"id":"approval","question":question,"header":tool,"body":body,"allow_other":false,"options":options}]}),panel))
}

fn native_body(panel:&Panel)->String {
    let content=panel.body.iter().map(|r|if rule(r,'╌'){""}else{r.as_str()}).collect::<Vec<_>>().join("\n");
    format!("{}\n\n{}",panel.title,content.trim()).trim().to_owned()
}
fn native_options(panel:&Panel)->Vec<Value> {
    panel.labels.iter().enumerate().map(|(i,text)|{
        let text=label(text);
        json!({"id":(i+1).to_string(),"label":text,"scope":if ONCE.contains(&text.as_str()){"once"}else{"broader"}})
    }).collect()
}
/// `body` followed by the warning of a visible native safety panel, if any.
fn with_warning(body:String,screen:Option<&str>)->String {
    let warning=screen.and_then(safety_panel).map(|p|p.body.iter().map(|r|if rule(r,'╌'){""}else{r.as_str()}).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    if warning.is_empty(){body}else{format!("{body}\n\n{}",warning.trim())}
}

/// What a request asks for, in full: a panel that does not show it completely
/// cannot. Bash shows its command and description, then any other field as
/// JSON; other tools show their whole input. None when it is too long to review.
fn request_body(request:&Value)->Option<String> {
    let tool=string(request,"tool_name");
    let mut input=request["tool_input"].as_object()?.clone();
    let mut parts=Vec::new();
    if tool=="Bash" {
        let command=input.remove("command").and_then(|c|c.as_str().map(str::to_owned)).filter(|c|!c.trim().is_empty())?;
        parts.push(command);
        if let Some(description)=input.remove("description").and_then(|d|d.as_str().map(str::to_owned)).filter(|d|!d.is_empty()) {parts.push(description);}
    }
    if !input.is_empty()||tool!="Bash" {parts.push(serde_json::to_string_pretty(&input).ok()?);}
    let body=format!("{tool}\n\n{}",parts.join("\n\n"));
    (body.chars().count()<=16_000).then_some(body)
}

const FEEDBACK:&str="Tell Claude what to do instead";

/// Whether a complete native panel shows the whole operation that the hook
/// grants. The legacy match accepts a file name or host, so it can be another
/// request with the same name, queued in front of this one. Only a Bash panel
/// can show it all: the main agent's own title, the full command, its
/// description if any, and no other input field. The caller also requires the
/// bound call to be the only active Bash call (`channel::alone`): the command
/// match ignores how whitespace wraps.
fn proves(request:&Value,panel:&Panel)->bool {
    let input=&request["tool_input"];
    request["tool_name"]=="Bash"&&panel.title=="Bash command"
        &&input.as_object().is_some_and(|fields|fields.keys().all(|key|key=="command"||key=="description"))
        &&matches(request,panel)
        &&compact(&panel.body.join("\n")).contains(&compact(string(input,"description")))
}

/// Any native diagnostic in a gutter between a visible chooser's last fence
/// and its question, whatever its text says.
fn diagnostic(screen:&str)->bool {
    let rows:Vec<_>=screen.lines().map(str::trim).collect();
    chooser(&rows).is_some_and(|(question,_,_)|rows[..question].iter().rposition(|r|rule(r,'╌')||rule(r,'─'))
        .is_some_and(|fence|rows[fence+1..question].iter().any(|r|r.starts_with("│ ")&&!gutter(r).is_empty())))
}

/// The visible native safety panel, and whether it belongs to this Bash
/// request: a panel that `proves` it, or, while the bound call is the only
/// active Bash call (`alone`), a clipped one whose visible command rows end
/// this request's command. Another request's warning is never shown as this
/// one's.
fn safety_warning(request:&Value,screen:&str,proven:bool,alone:bool)->Option<(Panel,bool)> {
    let panel=safety_panel(screen)?;
    if request["tool_name"]!="Bash" {return Some((panel,false));}
    if proven {return Some((panel,true));}
    let own=alone&&panel.body.iter().position(|row|rule(row,'╌')).is_some_and(|fence|{
        let shown=compact(&panel.body[..fence].iter().map(|row|row.strip_prefix('│').unwrap_or(row)).collect::<String>());
        !shown.is_empty()&&compact(string(&request["tool_input"],"command")).ends_with(&shown)
    });
    Some((panel,own))
}
/// The diagnostic rows of a safety panel: those below its last fence.
fn warning_text(panel:&Panel)->String {
    let start=panel.body.iter().rposition(|row|rule(row,'╌')).map_or(0,|fence|fence+1);
    panel.body[start..].join("\n").trim().to_owned()
}

/// The card for the pending main-agent request while Claude's permission hook
/// waits for it (`identity`). The hook gives the decision to the bound tool use
/// itself, so the card always shows that whole request. Only a panel that
/// `proves` the same operation adds Claude's options; one reaching beyond this
/// request goes through that verified panel. A complete panel that proves less
/// is left to the screen path, which acts on what it shows. A clipped one gives
/// a one-time Yes or No. None when the screen proves nothing, when a child's
/// request may be the one shown (`channel::shown`), or when a visible safety
/// warning cannot be shown as this request's own.
fn channel_card(record:&Value,screen:Option<&str>,identity:&Value)->Option<Value> {
    let screen=screen?;
    let request=request(record)?;
    if !eligible(record)||!channel::shown(record,screen) {return None;}
    let alone=channel::alone(record,string(identity,"tool_call_id"));
    let complete=parse(screen);
    if complete.as_ref().is_some_and(|panel|!proves(request,panel)||!alone) {return None;}
    let native=complete;
    // A diagnostic this card cannot show as the request's own leaves it to the
    // screen, whatever the diagnostic says.
    let warning=match safety_warning(request,screen,native.is_some(),alone) {
        Some((panel,true)) => Some(panel),
        Some((_,false)) => return None,
        None if diagnostic(screen) => return None,
        None => None,
    };
    let tool=string(request,"tool_name");
    let mut body=request_body(request)?;
    if let Some(warning)=&warning {body=format!("{body}\n\n{}",warning_text(warning));}
    let (question,options)=match &native {
        Some(panel) => (panel.question.clone(),native_options(panel)),
        None => (format!("Claude asks to use {tool}"),
            vec![json!({"id":"1","label":"Yes","scope":"once"}),json!({"id":"2","label":"No","scope":"once"})]),
    };
    // The shown warning is part of the review; its countdown is not.
    let hash=format!("{:x}",Sha256::digest(json!([identity["question_hash"],
        native.as_ref().map(|p|json!([p.title,identity_body(p),p.question,p.labels])),
        warning.as_ref().map(identity_body)]).to_string()));
    let created=record["pending_approval_at"].as_f64().map_or(record["last_event_at"].clone(),|at|json!(at));
    Some(json!({"question_id":identity["question_id"],"question_hash":hash,"tool_call_id":identity["tool_call_id"],
        "agent_id":request["agent_id"],"child_request_id":"","run_id":record["run_id"],"conversation_id":record["conversation_id"],
        "source":"claude_tool_approval","can_answer":true,"answer_unavailable_reason":"","approval":true,"approval_choices":true,
        "answer_transport":"claude_hook","panel_verified":native.is_some(),"created_at":created,
        "questions":[{"id":"approval","question":question,"header":tool,"body":body,"allow_other":true,
            "other_label":FEEDBACK,"other_description":"Deny this request with your message; Claude continues without it",
            "options":options}]}))
}

fn bash_title(title:&str)->bool {
    title=="Bash command"||title.strip_prefix("Bash command · from the ").is_some_and(|origin|
        origin.strip_suffix(" agent").is_some_and(|name|!name.trim().is_empty())
        ||origin.strip_prefix('"').and_then(|s|s.strip_suffix("\" workflow")).is_some_and(|name|!name.trim().is_empty()))
}

/// Native Bash safety checks can be visible while hook state still says
/// Working. Only the complete native chooser/footer plus a diagnostic gutter
/// establish this read-only fallback; ordinary output and composer drafts do
/// not. A clipped viewport cannot establish a command's exact identity.
fn safety_panel(screen:&str)->Option<Panel> {
    let rows:Vec<_>=screen.lines().map(str::trim).collect();
    let (question,labels,selected)=chooser(&rows)?;
    if rows[question]!="Do you want to proceed?"||labels!=["Yes","No"]
        ||!rows[question+1..].iter().any(|r|r.starts_with("Esc to cancel · Tab to amend")) {return None;}
    let fence=rows[..question].iter().rposition(|r|rule(r,'╌')||rule(r,'─'))?;
    if !rows[fence+1..question].iter().any(|r|r.starts_with("│ ")&&!gutter(r).is_empty()) {return None;}
    if let Some(panel)=parse(screen).filter(|p|bash_title(&p.title)) {return Some(panel);}
    // The long native panel may have scrolled its title/command out of view.
    // Its known dangerous-rm diagnostic still needs attention, but cannot be
    // answered here without the complete hook-backed request and panel.
    if !rows[fence+1..question].iter().any(|r|r.starts_with("│ Dangerous rm operation ")) {return None;}
    let start=rows[..fence].iter().position(|r|r.starts_with("│ ")).unwrap_or(fence+1);
    let body=rows[start..question].iter().map(|r|gutter(r).to_owned()).collect();
    Some(Panel{title:"Bash safety confirmation".into(),body,question:rows[question].into(),labels,selected})
}

fn safety_card(record:&Value,screen:&str)->Option<Value> {
    if record["agent"]!="claude"||!active_phase(record)
        ||pause_active(record)||!string(record,"error").is_empty()||string(record,"run_id").is_empty()
        ||string(record,"conversation_id").is_empty()||record.get("exited_at").is_some()||record.get("session_end").is_some() {return None;}
    let panel=safety_panel(screen)?;
    let hash=format!("{:x}",Sha256::digest(json!([record["run_id"],record["conversation_id"],record["pane"],record["pid"],record["process_start"],
        record["turn_started"],panel.title,identity_body(&panel),panel.question,panel.labels]).to_string()));
    let body=panel.body.iter().map(|r|if rule(r,'╌'){""}else{r.as_str()}).collect::<Vec<_>>().join("\n");
    Some(json!({"question_id":format!("claude-safety:{hash}"),"question_hash":hash,"tool_call_id":"",
        "run_id":record["run_id"],"conversation_id":record["conversation_id"],"source":"claude_terminal_safety",
        "approval":true,"approval_choices":false,"can_answer":false,"answer_transport":"",
        "answer_unavailable_reason":"Claude requires a safety confirmation. Review the complete command and answer in Terminal; Zerus has no matching native permission request.",
        "created_at":record["last_event_at"],"questions":[{"id":"safety","question":panel.question,"header":"Bash safety confirmation",
            "body":format!("{}\n\n{}",panel.title,body.trim()),"multi_select":false,"allow_other":false,
            "options":panel.labels.iter().enumerate().map(|(i,label)|json!({"id":(i+1).to_string(),"label":label,"scope":"once"})).collect::<Vec<_>>()}]}))
}

/// A chooser's title, body and (label, description) options.
type Chooser=(String,String,Vec<(String,String)>);
/// The chooser Claude draws at the bottom of its screen: a title under the
/// nearest full-width rule, optional body rows, then consecutive numbered
/// options with exactly one highlighted. Deeper indented rows describe the
/// option above them; blank rows may separate options.
fn bottom_choice(screen:&str)->Option<Chooser> {
    let rows:Vec<&str>=screen.lines().map(str::trim_end).collect();
    let marked=rows.iter().rposition(|r|numbered(r.trim()).is_some_and(|(mark,_,_)|mark))?;
    let top=rows[..marked].iter().rposition(|r|rule(r.trim(),'─'))?;
    let first=(top+1..=marked).find(|&i|numbered(rows[i].trim()).is_some())?;
    let text:Vec<&str>=rows[top+1..first].iter().map(|r|{let r=r.trim();r.strip_prefix('│').map_or(r,str::trim_start)}).collect();
    let title=text.iter().position(|r|!r.is_empty())?;
    let mut options:Vec<(String,String,usize)>=Vec::new();
    let (mut highlighted,mut index)=(0,first);
    while let Some(row)=rows.get(index) {
        let trimmed=row.trim();
        if trimmed.is_empty() {
            let next=rows[index+1..].iter().find(|r|!r.trim().is_empty());
            if next.and_then(|r|numbered(r.trim())).is_some_and(|(_,n,_)|n==options.len()+1) {index+=1;continue;}
            break;
        }
        if rule(trimmed,'─') {break;}
        match numbered(trimmed) {
            Some((mark,n,text)) if n==options.len()+1 => {
                highlighted+=usize::from(mark);
                let column=row.chars().count()-text.chars().count();
                options.push((text,String::new(),column));
            }
            Some(_) => return None,
            None => {
                let option=options.last_mut()?;
                if row.chars().count()-trimmed.chars().count()<option.2 {break;}
                if !option.1.is_empty() {option.1.push(' ');}
                option.1.push_str(trimmed);
            }
        }
        index+=1;
    }
    if highlighted!=1||options.len()<2||marked>=index {return None;}
    Some((text[title].to_owned(),text[title+1..].join("\n").trim().to_owned(),
        options.into_iter().map(|(label,description,_)|(label,description)).collect()))
}

/// Any chooser Claude draws for the user now: a tool approval or another
/// numbered panel at the bottom of its screen.
pub(super) fn chooser_visible(screen:&str)->bool {visible(screen)||bottom_choice(screen).is_some()}

// Every option of one question is on screen as its numbered row; a long label
// may wrap, so the row shows its beginning.
fn shows(screen:&str,items:&Value)->bool {
    let rows:Vec<_>=screen.lines().filter_map(|r|numbered(r.trim())).map(|(_,n,text)|(n,label(&text))).collect();
    items.as_array().into_iter().flatten().any(|item|{
        let options=item["options"].as_array().map_or(&[][..],Vec::as_slice);
        !options.is_empty()&&options.iter().enumerate().all(|(i,option)|rows.iter()
            .any(|(n,text)|*n==i+1&&!text.is_empty()&&label(string(option,"label")).starts_with(text.as_str())))
    })
}

/// What Claude asks in Terminal without a request Zerus can answer: its own
/// question, else any visible chooser while no other card explains the wait.
pub(super) fn terminal_card(record:&Value,cards:&[Value])->Option<Value> {
    if record["agent"]!="claude"||!["input","approval"].contains(&string(record,"phase")) {return None;}
    let screen=kimi_tui_choice::identity(record).and_then(|record|kimi_tui_choice::capture(&record)).ok();
    native_question(record,screen.as_deref())
        .or_else(||if cards.is_empty() {terminal_choice(record,screen.as_deref()?)} else {None})
}

/// Any other chooser while Claude waits for the user, such as a session paused
/// after a safeguard stop. No hook describes it, so the card shows the visible
/// panel verbatim and points to Terminal.
fn terminal_choice(record:&Value,screen:&str)->Option<Value> {
    if record["agent"]!="claude"||!["input","approval"].contains(&string(record,"phase"))
        ||pause_active(record)||!string(record,"error").is_empty() {return None;}
    let (title,body,options)=bottom_choice(screen)?;
    let identity=parse(screen).map(|p|json!([identity_body(&p),p.question])).unwrap_or_else(||json!(body));
    let hash=format!("{:x}",Sha256::digest(json!([record["run_id"],record["conversation_id"],record["pane"],record["pid"],
        record["process_start"],title,identity,options]).to_string()));
    let created=record["attention_started"].as_f64().map_or(record["last_event_at"].clone(),|at|json!(at));
    let options:Vec<_>=options.iter().enumerate()
        .map(|(i,(label,description))|json!({"id":(i+1).to_string(),"label":label,"description":description})).collect();
    Some(json!({"question_id":format!("claude-terminal-choice:{hash}"),"question_hash":hash,"tool_call_id":"",
        "run_id":record["run_id"],"conversation_id":record["conversation_id"],"source":"claude_terminal_choice",
        "can_answer":false,"answer_transport":"","answer_unavailable_reason":"Answer this in Terminal.","created_at":created,
        "questions":[{"id":"terminal","question":title,"header":"Terminal","body":body,"multi_select":false,
            "allow_other":false,"options":options}]}))
}

/// Claude Code asks some questions itself, such as a model switch after a
/// safeguard stop. They arrive only as an AskUserQuestion PermissionRequest:
/// without a tool call there is no tool result to acknowledge an answer, so
/// the card mirrors the request and points to Terminal. A readable screen must
/// still show its options. A model question has an active tool call and keeps
/// claude_question's answerable card.
fn native_question(record:&Value,screen:Option<&str>)->Option<Value> {
    let request=&record["pending_approval"];
    let tool=string(request,"tool_name");
    if record["agent"]!="claude"||record["phase"]!="input"||pause_active(record)||!string(record,"error").is_empty()
        ||!request.is_object()||!question_tool(tool)
        ||record["active_tools"].as_object().into_iter().flatten().any(|(_,active)|active["name"]==tool) {return None;}
    let mut items=questions::normalize(&request["tool_input"]["questions"]).unwrap_or_else(|_|json!([{"id":"q_0",
        "question":"Claude asks a question in Terminal","header":tool,"body":summary(request),"options":[]}]));
    if screen.is_some_and(|screen|!shows(screen,&items)) {return None;}
    for item in items.as_array_mut().into_iter().flatten() {item["allow_other"]=json!(false);}
    let hash=format!("{:x}",Sha256::digest(json!([record["run_id"],record["conversation_id"],record["pane"],record["pid"],
        record["process_start"],tool,request["tool_input"]]).to_string()));
    let created=record["pending_approval_at"].as_f64().map_or(record["last_event_at"].clone(),|at|json!(at));
    Some(json!({"question_id":format!("claude-native-question:{hash}"),"question_hash":hash,"tool_call_id":"",
        "run_id":record["run_id"],"conversation_id":record["conversation_id"],"source":"claude_native_question",
        "can_answer":false,"answer_transport":"","answer_unavailable_reason":"Claude Code asks this itself. Answer it in Terminal.",
        "created_at":created,"questions":items}))
}

fn snapshot(record:&Value,screen:&str)->Option<(Value,usize)> {
    let (card,panel)=build(record,Some(screen))?;
    if card["can_answer"]!=true {return None;}
    Some((card["question_hash"].clone(),panel?.selected))
}
fn acknowledged(screen:&str)->bool {!screen.trim().is_empty()&&!visible(screen)}

pub(super) fn current(record:&Value)->Option<Value> {current_with(record,false)}
/// The card a question listing shows. Only a listing may first fit a clipped
/// panel into its unviewed pane, and only for a recorded request; guards and
/// answers read the screen as it is.
pub(super) fn listed(record:&Value)->Option<Value> {current_with(record,true)}
// The `fit` the last approval read on this thread received (tests only):
// questions' tests pin which reads are listings.
#[cfg(test)]
thread_local! {pub(super) static LAST_FIT:std::cell::Cell<Option<bool>>=const {std::cell::Cell::new(None)};}
fn current_with(record:&Value,fit:bool)->Option<Value> {
    #[cfg(test)] LAST_FIT.with(|last|last.set(Some(fit)));
    if record["agent"]!="claude"||pause_active(record)||!string(record,"error").is_empty()
        ||!active_phase(record) {return None;}
    let live=kimi_tui_choice::identity(record).ok();
    let mut screen=live.as_ref().and_then(|live|kimi_tui_choice::capture(live).ok());
    // A waiting permission hook needs no taller pane; a decision it took or
    // may have taken is never offered again, through any transport.
    if let Some(identity)=channel::identity(record) {
        match answer_channel::route(record,&identity) {
            Ok(answer_channel::Route::Channel(_)) => if let Some(card)=channel_card(record,screen.as_deref(),&identity) {return Some(card);},
            // The card keeps its identity, so an answer to it learns why.
            Ok(answer_channel::Route::Locked) => return channel_card(record,screen.as_deref(),&identity)
                .or_else(||screen_card(record,screen.as_deref())).map(|mut card|{
                card["can_answer"]=json!(false);card["approval_choices"]=json!(false);card["answer_transport"]=json!("");
                card["answer_unavailable_reason"]=json!(channel::LOCKED);card
            }),
            _ => {}
        }
    }
    if let Some(live)=live.as_ref().filter(|live|fit&&fits(live)) {
        screen=screen.map(|shown|question_terminal::fit_unviewed(live,shown,kimi_tui_choice::capture,clipped,complete_panel));
    }
    screen_card(record,screen.as_deref())
}
fn screen_card(record:&Value,screen:Option<&str>)->Option<Value> {
    build(record,screen).map(|(card,_)|card).or_else(||safety_card(record,screen?))
}
pub(super) fn available(record:&Value,question:&Value)->Result<()> {
    let current=current(record).ok_or("The native approval is no longer visible. Open Terminal")?;
    if current["question_hash"]!=question["question_hash"] {return Err("The approval changed. Review its new request".into());}
    if current["can_answer"]!=true {return Err(string(&current,"answer_unavailable_reason").to_owned());}
    Ok(())
}
/// Deliver `answers` to the approval `question` and name the transport used:
/// the keys of the verified panel (`claude_tui`), or Claude's waiting
/// permission hook (`claude_hook`) for a one-time decision on its card.
pub(super) fn answer(record:&Value,question:&Value,answers:&Value,request_id:&str)->Result<&'static str> {
    available(record,question)?;
    if question["answer_transport"]!="claude_hook" {keys(record,question,answers,snapshot)?;return Ok("claude_tui");}
    let identity=channel::identity(record).ok_or("The approval changed. Review its new request")?;
    let dir=match answer_channel::route(record,&identity)? {
        answer_channel::Route::Channel(dir) => dir,
        answer_channel::Route::Locked => return Err(channel::LOCKED.into()),
        answer_channel::Route::Screen => return Err("Claude's permission hook stopped waiting. Nothing was sent; review the approval again.".into()),
    };
    let text=string(&answers[0],"text");
    let option=answers[0]["selected_option_ids"][0].as_str().and_then(|id|question["questions"][0]["options"].as_array()?
        .iter().find(|option|string(option,"id")==id));
    let decision=match option {
        _ if !text.is_empty() => "feedback",
        Some(option) if option["scope"]=="once" => match label(string(option,"label")) {
            once if once.starts_with("Yes") => "allow",
            once if once.starts_with("No") => "deny",
            _ => return Err("Choose one of Claude's options".into()),
        },
        // Broader permissions exist only in Claude's panel, behind its second confirmation.
        Some(_) => {keys(record,question,answers,channel_snapshot)?;return Ok("claude_tui");}
        None => return Err("Choose one of Claude's options".into()),
    };
    let offer=json!({"request_id":request_id,"decision":decision,"message":text});
    let on_screen=question["panel_verified"]==true&&decision!="feedback";
    let transport=answer_channel::deliver(&dir,&offer,std::time::Duration::from_secs(3),std::time::Duration::from_secs(3),||{
        if !on_screen {return Err("Claude's permission hook did not take the answer, and the panel cannot be answered on screen. Nothing was sent; answer it in Terminal.".into());}
        keys(record,question,answers,channel_snapshot)
    })?;
    if transport=="claude_hook" {
        let _ = settle(record,match decision {"allow"=>Settled::Approved,"deny"=>Settled::Interrupted,_=>Settled::Refused(string(&identity,"tool_call_id"))});
    }
    Ok(transport)
}
/// Choose option `answers` in the verified native panel with its keys.
fn keys(record:&Value,question:&Value,answers:&Value,snapshot:fn(&Value,&str)->Option<(Value,usize)>)->Result<()> {
    let screen=kimi_tui_choice::capture(record)?;
    let panel=parse(&screen).ok_or("Approval disappeared")?;
    let chosen=answers[0]["selected_option_ids"][0].as_str().and_then(|id|id.parse::<usize>().ok())
        .filter(|n|(1..=panel.labels.len()).contains(n)).ok_or("Choose one of Claude's options")?-1;
    let navigation_settle=if safety_panel(&screen).is_some(){std::time::Duration::from_millis(250)}else{std::time::Duration::ZERO};
    kimi_tui_choice::answer_queued(record,question,chosen,panel.labels.len(),snapshot,acknowledged,navigation_settle)?;
    if string(question,"child_request_id").is_empty() {
        let _ = settle(record,if label(&panel.labels[chosen]).starts_with("No") {Settled::Interrupted} else {Settled::Approved});
    } else {
        let _ = settle_child(record,string(question,"child_request_id"));
    }
    Ok(())
}
/// The hash and highlight of a channel card with its complete native panel.
fn channel_snapshot(record:&Value,screen:&str)->Option<(Value,usize)> {
    let card=channel_card(record,Some(screen),&channel::identity(record)?)?;
    if card["panel_verified"]!=true {return None;}
    Some((card["question_hash"].clone(),parse(screen)?.selected))
}
fn settle_child(original:&Value,id:&str)->Result<()> {
    let _guard=lock(None)?;
    if let Some(mut record)=read_run(string(original,"name"),string(original,"run_id"))? {
        if children::settle(&mut record,original,id) {write(&mut record)?;}
    }
    Ok(())
}
/// How an answered request ended: approved, denied with the turn, or denied
/// with feedback for the tool use named, after which Claude continues.
enum Settled<'a> {Approved,Interrupted,Refused(&'a str)}
/// Claude sends no hook when an approval is answered: a denial ends its turn,
/// and an approved tool reports PostToolUse only when it finishes. Record a
/// denial as an interruption so the composer can verify Claude's prompt again,
/// and an approval as the tool's work. A refused tool never runs and reports no
/// end. Only the same pending request settles.
fn settle(original:&Value,outcome:Settled)->Result<Option<Value>> {
    let _guard=lock(None)?;
    let Some(mut record)=read_run(string(original,"name"),string(original,"run_id"))? else {return Ok(None)};
    if record["pending_approval"]!=original["pending_approval"]||record["pending_approval_at"]!=original["pending_approval_at"]
        ||record["phase"]!="approval" {return Ok(None);}
    record.as_object_mut().unwrap().remove("pending_approval");
    record.as_object_mut().unwrap().remove("pending_approval_at");
    match outcome {
        Settled::Interrupted => {
            record["activity"]=json!("unknown");record["phase"]=json!("interrupted");
            record["main_done"]=json!(false);record["active_tools"]=json!({});
            channel::observe(&mut record,&json!({"hook_event_name":"Interrupt"}));
        }
        Settled::Approved|Settled::Refused(_) => {
            if let Settled::Refused(tool_use)=outcome {
                if let Some(tools)=record["active_tools"].as_object_mut() {tools.remove(tool_use);}
                channel::forget(&mut record,tool_use);
            }
            let tools=record["active_tools"].as_object().is_some_and(|tools|!tools.is_empty());
            record["phase"]=json!(if tools {"tool"} else {"working"});
        }
    }
    write(&mut record)?;
    Ok(Some(record))
}

// Claude prints this row below the tool it denied, as it does for Escape.
const INTERRUPTED:&str="Interrupted · What should Claude do instead?";
const BACKGROUND_HINT:&str="(ctrl+b to run in background)";

// Rows Claude may draw below its transcript, above the next full-width rule.
fn blank(row:&str)->bool {row.trim().is_empty()}
/// The summary of a finished turn, such as "✻ Sautéed for 1m 3s · done 7:47 PM".
fn finished(row:&str)->bool {
    static SUMMARY:std::sync::OnceLock<regex::Regex>=std::sync::OnceLock::new();
    let row=row.trim();
    !row.contains('…')&&SUMMARY.get_or_init(||regex::Regex::new(r"^[^\w\s] \w+ for [0-9]").unwrap()).is_match(row)
}
/// The spinner of a working turn, such as "✢ Actualizing… (3s · ↓ 52 tokens)".
fn spinning(row:&str)->bool {
    static SPINNER:std::sync::OnceLock<regex::Regex>=std::sync::OnceLock::new();
    SPINNER.get_or_init(||regex::Regex::new(r"^[^\w\s] \S.*…").unwrap()).is_match(row.trim())
}
/// A right-aligned indicator, such as the effort level above the composer.
fn aside(row:&str)->bool {row.len()-row.trim_start().len()>=20}

/// The rows from `start` up to the next full-width rule, Claude's composer or
/// a later panel, all match `trailing`: nothing newer follows in the transcript.
fn last_in_transcript(rows:&[&str],start:usize,mut trailing:impl FnMut(&str)->bool)->bool {
    rows[start..].iter().position(|row|rule(row.trim(),'─'))
        .is_some_and(|end|rows[start..start+end].iter().all(|row|blank(row)||trailing(row)))
}

/// Claude ended the turn on this denial: its interruption row is the last row
/// of the transcript. A new turn would show its prompt, tool or spinner below.
fn denied(screen:&str)->bool {
    let rows:Vec<&str>=screen.lines().collect();
    rows.iter().rposition(|row|row.trim().strip_prefix('⎿').is_some_and(|text|text.trim()==INTERRUPTED))
        .is_some_and(|row|last_in_transcript(&rows,row+1,|row|finished(row)||aside(row)))
}

/// The approved command runs: Claude follows the request's own command row
/// with its elapsed time, such as "(3s)" or "(4s · 5 lines)", only while the
/// command runs. Below it, only the background hint and the spinner remain.
/// The spinner may draw indented rows under itself, such as a tip; the
/// transcript never continues below the spinner.
fn running(request:&Value,screen:&str)->bool {
    static ELAPSED:std::sync::OnceLock<regex::Regex>=std::sync::OnceLock::new();
    let elapsed=ELAPSED.get_or_init(||regex::Regex::new(r"^(?:(.*) )?\((?:[0-9]+h )?(?:[0-9]+m )?[0-9]+s(?: · [^()]*)?\)$").unwrap());
    let command=string(&request["tool_input"],"command");
    if request["tool_name"]!="Bash"||command.trim().is_empty() {return false;}
    let rows:Vec<&str>=screen.lines().collect();
    let shown=|row:&str|row.trim().strip_prefix('⎿').and_then(|text|text.trim_start().strip_prefix('$'))
        .and_then(|text|text.strip_prefix([' ','\u{a0}'])).map(str::to_owned);
    let Some(start)=rows.iter().rposition(|row|shown(row).is_some()) else {return false};
    let mut text=String::new();
    for (index,row) in rows.iter().enumerate().skip(start) {
        let row=if index==start {shown(row).unwrap_or_default()} else {row.trim().to_owned()};
        if rule(&row,'─') {return false;}
        if let Some(found)=elapsed.captures(&row) {
            text.push_str(found.get(1).map_or("",|part|part.as_str()));
            let mut spinner=false;
            return compact(&text)==compact(command)
                &&last_in_transcript(&rows,index+1,|row|{
                    if spinner {return row.starts_with(char::is_whitespace);}
                    spinner=spinning(row);
                    spinner||row.trim()==BACKGROUND_HINT||aside(row)
                });
        }
        text.push_str(&row);text.push('\n');
    }
    false
}

/// How the pending request was answered in Terminal, when the screen proves
/// it: Some(false) for a denial, Some(true) for the approved command running.
/// A visible chooser, or a screen that may precede the panel, proves nothing.
fn answered(record:&Value,screen:&str)->Option<bool> {
    let request=request(record)?;
    if screen.trim().is_empty()||visible(screen) {None}
    else if denied(screen) {Some(false)}
    else if running(request,screen) {Some(true)}
    else {None}
}

/// A main-agent request still at Needs approval. Listings, inspection and
/// `hgs read` reconcile every session; only this one captures its terminal.
fn pending(record:&Value)->bool {eligible(record)&&request(record).is_some()}

/// Settle a request answered in Terminal, as an Activity answer does. Without
/// proof on the screen the session stays at Needs approval until a hook.
pub(super) fn reconcile(record:Value)->Value {
    if !pending(&record) {return record;}
    let screen=kimi_tui_choice::identity(&record).and_then(|current|kimi_tui_choice::capture(&current));
    match screen.ok().and_then(|screen|answered(&record,&screen)) {
        Some(approved) => settle(&record,if approved {Settled::Approved} else {Settled::Interrupted}).ok().flatten().unwrap_or(record),
        None => record,
    }
}

#[cfg(test)] mod tests {
    use super::*;
    const SAFETY:&str=include_str!("../../tests/fixtures/claude-bash-safety.txt");
    // Captured from Claude Code 2.1.294 at 120 columns; paths are synthetic.
    const BASH:&str="  ⎿  $ printf hi > out.txt\n\n────────────────────────────────────────\n Bash command\n Tip: auto mode handles these prompts for you — choose \"switch to auto mode\" below\n Write \"hi\" into out.txt without a trailing newline\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n printf hi > out.txt\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and always allow access to\n      /work/project from this\n      project\n   3. Yes, and switch to auto mode · auto mode handles these prompts for you\n   4. No\n\n Esc to cancel · Tab to amend\n\n\n\n";
    const WRITE:&str="● Write(new.txt)\n\n────────────────────────────────────────\n Create file\n new.txt\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n  1 x\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Do you want to create new.txt?\n ❯ 1. Yes\n   2. Yes, and switch to accept edits (auto-approve file edits and common file commands) for this session (shift+tab)\n   3. No\n\n Esc to cancel · Tab to amend\n\n";
    const FETCH:&str="● Fetch(https://example.com)\n\n────────────────────────────────────────\n Fetch\n Claude wants to fetch content from example.com\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n url: https://example.com/\n prompt: title?\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Do you want to allow Claude to fetch this content?\n ❯ 1. Yes\n   2. Yes, and don't ask again for example.com\n   3. No, and tell Claude what to do differently (esc)\n\n\n\n";
    const OUTSIDE:&str="────────────────────────────────────────\n Read outside the working directories\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Read(/home/user/.codex/generated_images/0000aaaa-1111-2222-3333-444455556666/exec-77778888-9999-aaaa-bbbb-cc\n ccddddeeee.png)\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Auto mode and the sandbox read outside the working directories without asking. Yes or No answers for this read; the\n rest is for later reads. Block: the file tools refuse reads outside the working directories in every project.\n\n Allow this read outside the working directories?\n ❯ 1. Yes, and keep allowing any reads outside the working directories\n   2. No, and block reads outside the working directories from now on\n   3. No, and ask again next time\n   4. Yes, but ask again next time\n\n Esc to cancel · Tab to amend\n";
    const PATH:&str="/home/user/.codex/generated_images/0000aaaa-1111-2222-3333-444455556666/exec-77778888-9999-aaaa-bbbb-ccccddddeeee.png";
    const DENIED:&str="❯ Use the tool now.\n\n● Fetch(https://example.com)\n  ⎿  Interrupted · What should Claude do instead?\n\n────────────────────────────────────────\n❯ \n────────────────────────────────────────\n  ⏸ manual mode on · ? for shortcuts\n";
    fn record(tool:&str,input:Value)->Value {
        json!({"agent":"claude","phase":"approval","run_id":"run","conversation_id":"conversation","pending_approval_at":10.0,
            "active_tools":{"toolu_one":{"name":tool}},"pending_approval":{"hook_event_name":"PermissionRequest","tool_name":tool,"tool_input":input}})
    }
    fn card(record:&Value,screen:&str)->Value {build(record,Some(screen)).unwrap().0}
    fn options(card:&Value)->Vec<(String,String)> {
        card["questions"][0]["options"].as_array().unwrap().iter().map(|o|(string(o,"label").to_owned(),string(o,"scope").to_owned())).collect()
    }

    fn child(record:&mut Value,id:&str,command:&str) {
        questions::observe(record,&json!({"hook_event_name":"PermissionRequest","agent_id":id,
            "tool_name":"Bash","tool_input":{"command":command}}));
    }
    #[test] fn workflow_request_binds_only_one_exact_child_and_refreshes_identity() {
        let mut r=json!({"agent":"claude","phase":"working","run_id":"run","conversation_id":"conversation"});
        let command="T=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port";
        let screen=SAFETY.replace("Bash command","Bash command · from the \"fixture-checks\" workflow");
        child(&mut r,"child-a",command);
        let q=card(&r,&screen);
        assert_eq!(q["can_answer"],true);assert_eq!(q["agent_id"],"child-a");
        assert!(!string(&q,"child_request_id").is_empty());assert_eq!(q["tool_call_id"],"");
        assert_eq!(q["question_hash"],card(&r,&screen.replace("in 2:00,","in 1:59,"))["question_hash"]);
        assert!(safety_card(&r,&screen).is_some());
        r["phase"]=json!("idle");
        assert_eq!(q["question_hash"],card(&r,&screen)["question_hash"]);
        assert!(safety_card(&r,&screen).is_some());
        child(&mut r,"child-b","printf other");
        assert_eq!(q["question_hash"],card(&r,&screen)["question_hash"]);
        child(&mut r,"child-c",command);
        assert_eq!(card(&r,&screen)["can_answer"],false);
        assert!(snapshot(&r,&screen).is_none());
        questions::observe(&mut r,&json!({"hook_event_name":"SubagentStop","agent_id":"child-c"}));
        assert_eq!(q["question_hash"],card(&r,&screen)["question_hash"]);
        questions::observe(&mut r,&json!({"hook_event_name":"PostToolUse","agent_id":"child-a","tool_name":"Bash","tool_input":{"command":command}}));
        child(&mut r,"child-a",command);
        assert_ne!(q["question_hash"],card(&r,&screen)["question_hash"]);
    }
    #[test] fn child_completion_and_settlement_preserve_parent_and_siblings() {
        let mut r=record("Bash",json!({"command":"parent"}));
        let parent=r["pending_approval"].clone();let tools=r["active_tools"].clone();
        child(&mut r,"child-a","printf hi > out.txt");child(&mut r,"child-b","other");
        let q=card(&r,BASH);assert_eq!(q["can_answer"],true);
        let saved=r.clone();
        // A completion for another command of the same child cannot clear it.
        questions::observe(&mut r,&json!({"hook_event_name":"PostToolUse","agent_id":"child-a","tool_name":"Bash","tool_input":{"command":"other"}}));
        assert_eq!(q["question_hash"],card(&r,BASH)["question_hash"]);
        assert!(children::settle(&mut r,&saved,string(&q,"child_request_id")));
        assert!(!children::settle(&mut r,&saved,string(&q,"child_request_id")));
        assert_eq!(r["pending_approval"],parent);assert_eq!(r["active_tools"],tools);assert_eq!(r["phase"],"approval");
        assert_eq!(children::entries(&r).count(),1);
        for kind in ["Stop","PostToolUse","UserPromptSubmit"] {
            questions::observe(&mut r,&json!({"hook_event_name":kind}));
            assert_eq!(children::entries(&r).count(),1);
        }
        questions::observe(&mut r,&json!({"hook_event_name":"SessionStart"}));
        assert_eq!(children::entries(&r).count(),0);
    }
    #[test] fn duplicate_parent_child_requests_and_missing_proof_never_answer() {
        let mut r=record("Bash",json!({"command":"printf hi > out.txt"}));
        child(&mut r,"child-a","printf hi > out.txt");
        assert_eq!(card(&r,BASH)["can_answer"],false);
        r.as_object_mut().unwrap().remove("pending_approval");
        r["phase"]=json!("working");
        let clipped=BASH.split_once("Bash command\n").unwrap().1;
        assert!(build(&r,Some(clipped)).is_none());assert!(build(&r,None).is_none());
        assert!(build(&r,Some(&BASH.replace("printf hi > out.txt","printf changed"))).is_none());
        for (key,value) in [("error",json!("error")),("session_end",json!({})),("exited_at",json!(1))] {
            let mut invalid=r.clone();invalid[key]=value;assert!(build(&invalid,Some(BASH)).is_none());
        }
        r["child_approval_overflow"]=json!(true);
        assert_eq!(card(&r,BASH)["can_answer"],false);
        questions::observe(&mut r,&json!({"hook_event_name":"SessionEnd"}));
        assert!(r.get("child_approval_overflow").is_none());
    }
    #[test] fn unsupported_child_tool_cannot_be_answered_as_the_parent() {
        let mut r=record("Read",json!({"file_path":PATH}));
        questions::observe(&mut r,&json!({"hook_event_name":"PermissionRequest","agent_id":"child",
            "tool_name":"Read","tool_input":{"file_path":PATH}}));
        assert_eq!(card(&r,OUTSIDE)["can_answer"],false);
        r.as_object_mut().unwrap().remove("pending_approval");
        assert_eq!(card(&r,OUTSIDE)["can_answer"],false);
    }

    #[test] fn generic_read_only_countdown_has_stable_identity() {
        let r=json!({"agent":"claude","phase":"input"});
        let q=terminal_choice(&r,SAFETY).unwrap();
        assert_eq!(q["question_hash"],terminal_choice(&r,&SAFETY.replace("in 2:00,","in 1:59,")).unwrap()["question_hash"]);
        assert_ne!(q["question_hash"],terminal_choice(&r,&SAFETY.replace("possibly-empty","possibly-wrong")).unwrap()["question_hash"]);
    }

    #[test] fn safety_gutter_matches_exact_command_and_preserves_the_warning() {
        let r=record("Bash",json!({"command":"T=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port"}));
        let q=card(&r,SAFETY);
        assert_eq!(q["can_answer"],true);
        assert!(string(&q["questions"][0],"body").contains("Dangerous rm operation on possibly-empty variable path:"));
        assert_eq!(options(&q),[("Yes".into(),"once".into()),("No".into(),"once".into())]);
        assert_eq!(q["question_hash"],card(&r,&SAFETY.replace("in 2:00,","in 1:59,"))["question_hash"]);
        let advisory="⚠ Claude Code will automatically deny this request in 2:00, to avoid blocking progress on an unattended session";
        let wrapped=SAFETY.replace(advisory,"⚠ Claude Code will automatically deny this\n request in 2:00, to avoid blocking progress on an\n unattended session");
        assert_eq!(q["question_hash"],card(&r,&wrapped)["question_hash"]);
        assert_eq!(q["question_hash"],card(&r,&wrapped.replace("in 2:00,","in 1:59,"))["question_hash"]);
        assert_eq!(q["question_hash"],card(&r,&SAFETY.replace(advisory,""))["question_hash"]);
        assert_ne!(q["question_hash"],card(&r,&SAFETY.replace(advisory,&format!("{advisory} Changed warning")))["question_hash"]);
        let current_advisory="⚠ Claude Code will automatically block this request in 2:00, to avoid holding up an unattended session";
        let current=SAFETY.replace(advisory,current_advisory);
        assert_eq!(q["question_hash"],card(&r,&current)["question_hash"]);
        assert_eq!(q["question_hash"],card(&r,&current.replace("in 2:00,","in 1:59,"))["question_hash"]);
        assert!(string(&card(&r,&current)["questions"][0],"body").contains(current_advisory));
        assert_ne!(q["question_hash"],card(&r,&current.replace("holding up","blocking progress"))["question_hash"]);
        let mut command_panel=parse(SAFETY).unwrap();
        let fence=command_panel.body.iter().rposition(|row|rule(row,'╌')).unwrap();
        command_panel.body.insert(fence,advisory.into());
        assert!(identity_body(&command_panel).contains(&advisory.to_owned()));
        assert_ne!(q["question_hash"],card(&r,&SAFETY.replace("possibly-empty","possibly-wrong"))["question_hash"]);
        let other=record("Bash",json!({"command":"rm other"}));
        let other=card(&other,SAFETY);assert_eq!(other["can_answer"],false);
        assert!(string(&other["questions"][0],"body").contains("Dangerous rm operation"));
    }

    #[test] fn native_safety_without_permission_hook_is_visible_but_read_only() {
        let r=json!({"agent":"claude","phase":"working","run_id":"run","conversation_id":"conversation","last_event_at":10});
        let q=safety_card(&r,SAFETY).unwrap();
        assert_eq!(q["source"],"claude_terminal_safety");assert_eq!(q["can_answer"],false);
        assert_eq!(q["approval"],true);assert_eq!(q["questions"][0]["options"].as_array().unwrap().len(),2);
        assert!(string(&q["questions"][0],"body").contains("Dangerous rm operation"));
        assert_eq!(q["question_hash"],safety_card(&r,&SAFETY.replace("in 2:00,","in 1:59,")).unwrap()["question_hash"]);
        let moved=SAFETY.replace(" ❯ 1. Yes","   1. Yes").replace("   2. No"," ❯ 2. No");
        assert_eq!(q["question_hash"],safety_card(&r,&moved).unwrap()["question_hash"]);
        for (key,value) in [("run_id",json!("different")),("conversation_id",json!("different")),("turn_started",json!(20))] {
            let mut other=r.clone();other[key]=value;
            assert_ne!(q["question_hash"],safety_card(&other,SAFETY).unwrap()["question_hash"]);
        }
        for (key,value) in [("phase",json!("idle")),("error",json!("failed")),("exited_at",json!(12))] {
            let mut other=r.clone();other[key]=value;assert!(safety_card(&other,SAFETY).is_none());
        }
    }

    #[test] fn clipped_safety_keeps_the_diagnostic_without_approving_partial_commands() {
        let first=SAFETY.find(" │ T=$(pwd)").unwrap();let clipped=&SAFETY[first..];
        assert!(parse(clipped).is_none());
        let r=record("Bash",json!({"command":"T=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port"}));
        let q=card(&r,clipped);assert_eq!(q["can_answer"],false);
        assert!(string(&q["questions"][0],"body").contains("Dangerous rm operation"));
        assert!(safety_card(&r,clipped).is_some());
        for invalid in [
            format!("{SAFETY}\n────────────────────────────────────────\n❯ \n────────────────────────────────────────"),
            SAFETY.replace("Esc to cancel · Tab to amend","Ordinary output"),
            SAFETY.replace(" ❯ 1. Yes","   1. Yes"),
            SAFETY.replace("   2. No"," ❯ 2. No"),
            SAFETY.replace("   2. No","   3. No"),
            SAFETY.replace("Do you want to proceed?","Quoted question?"),
            SAFETY.replace("   2. No","   2. Yes, always allow"),
        ] {assert!(safety_card(&r,&invalid).is_none());}
    }

    #[test] fn clipped_panel_grows_only_an_unviewed_pane_for_a_recorded_request() {
        use std::sync::atomic::{AtomicBool,AtomicU16,Ordering::SeqCst};
        static HEIGHT:AtomicU16=AtomicU16::new(24);static VIEWED:AtomicBool=AtomicBool::new(false);
        static SHORT:AtomicBool=AtomicBool::new(false);static GROWN:AtomicU16=AtomicU16::new(0);
        // A clipped panel fills the pane down to Claude's blank last row.
        fn clipped_screen()->String {
            let tail=SAFETY[SAFETY.find(" │ T=$(pwd)").unwrap()..].trim_end();
            let height=usize::from(HEIGHT.load(SeqCst));
            format!("{}{tail}\n\n"," │ :\n".repeat(height.saturating_sub(tail.lines().count()+1)))
        }
        struct Pane;
        impl backend::TerminalBackend for Pane {
            fn pane_info(&self,_:backend::Target,_:&[backend::PaneField])->Result<backend::PaneInfo> {
                Ok(backend::PaneInfo::new(format!("{}\n",HEIGHT.load(SeqCst))))
            }
            fn grow_unviewed(&self,pane:&str,rows:u16)->Result<bool> {
                assert_eq!(pane,"%7");
                if VIEWED.load(SeqCst) {return Ok(false);}
                HEIGHT.store(rows,SeqCst);GROWN.fetch_add(1,SeqCst);Ok(true)
            }
            // Claude redraws its whole panel once the pane is tall enough; a
            // panel taller than any allowed pane stays clipped at the bottom.
            fn capture(&self,_:&str,_:backend::Capture)->Result<Vec<u8>> {
                if HEIGHT.load(SeqCst)>=48&&!SHORT.load(SeqCst) {return Ok(SAFETY.as_bytes().to_vec());}
                Ok(clipped_screen().into_bytes())
            }
        }
        let reset=|viewed,short|{HEIGHT.store(24,SeqCst);VIEWED.store(viewed,SeqCst);SHORT.store(short,SeqCst);GROWN.store(0,SeqCst);};
        let mut r=record("Bash",json!({"command":"T=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port"}));
        r["pane"]=json!("%7");
        let complete=|r:&Value,screen:String|question_terminal::grow_until(r,screen,kimi_tui_choice::capture,clipped,complete_panel);
        assert!(fits(&r));
        let mut hookless=r.clone();hookless.as_object_mut().unwrap().remove("pending_approval");
        assert!(!fits(&hookless));
        backend::with_fake(&Pane,||{
            reset(false,false);
            let screen=complete(&r,clipped_screen());
            assert_eq!((GROWN.load(SeqCst),HEIGHT.load(SeqCst)),(1,48));
            assert_eq!(card(&r,&screen)["can_answer"],true);
            // A complete panel never resizes the pane.
            reset(false,false);
            assert_eq!(complete(&r,SAFETY.into()),SAFETY);
            assert_eq!(GROWN.load(SeqCst),0);
            // A client's size is the user's: the clipped request stays in Terminal.
            reset(true,false);
            let screen=complete(&r,clipped_screen());
            assert_eq!((GROWN.load(SeqCst),HEIGHT.load(SeqCst)),(0,24));
            assert_eq!(card(&r,&screen)["can_answer"],false);
            // Growth is bounded; a panel that never fits is never answered.
            reset(false,true);
            let screen=complete(&r,clipped_screen());
            assert_eq!((GROWN.load(SeqCst),HEIGHT.load(SeqCst)),(3,question_terminal::MAX_UNVIEWED_ROWS));
            assert!(clipped(&screen));assert_eq!(card(&r,&screen)["can_answer"],false);
        });
    }

    fn hooked(tool:&str,input:Value)->Value {
        let mut r=record(tool,input.clone());
        channel::observe(&mut r,&json!({"hook_event_name":"PreToolUse","tool_use_id":"toolu_one","tool_name":tool,"tool_input":input}));
        r["claude_tool_uses"]["toolu_one"]["at"]=json!(5.0);
        r
    }
    #[test] fn waiting_hook_card_answers_once_without_a_complete_panel() {
        let command="T=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port";
        let r=hooked("Bash",json!({"command":command}));
        let identity=channel::identity(&r).unwrap();
        assert_eq!(identity["tool_call_id"],"toolu_one");
        let clipped=&SAFETY[SAFETY.find(" │ T=$(pwd)").unwrap()..];
        assert_eq!(card(&r,clipped)["can_answer"],false,"the screen alone cannot answer it");
        let q=channel_card(&r,Some(clipped),&identity).unwrap();
        assert_eq!((q["can_answer"].clone(),q["answer_transport"].clone(),q["panel_verified"].clone()),(json!(true),json!("claude_hook"),json!(false)));
        assert_eq!((q["question_id"].clone(),q["tool_call_id"].clone()),(identity["question_id"].clone(),json!("toolu_one")));
        assert_eq!(options(&q),[("Yes".into(),"once".into()),("No".into(),"once".into())]);
        let item=&q["questions"][0];
        assert_eq!((item["allow_other"].clone(),item["other_label"].clone()),(json!(true),json!(FEEDBACK)));
        let body=string(item,"body");
        assert!(body.starts_with(&format!("Bash\n\n{command}")),"{body}");
        assert!(body.contains("Dangerous rm operation"),"{body}");
        // A complete panel of this request shows Claude's own options.
        let full=channel_card(&r,Some(SAFETY),&identity).unwrap();
        assert_eq!((full["panel_verified"].clone(),full["question_id"].clone()),(json!(true),q["question_id"].clone()));
        assert_ne!(full["question_hash"],q["question_hash"],"other options are another question");
        assert_eq!(channel_snapshot(&r,SAFETY).unwrap().0,full["question_hash"]);
        assert!(channel_snapshot(&r,clipped).is_none(),"only a verified panel takes keys");
        let moved=SAFETY.replace(" ❯ 1. Yes","   1. Yes").replace("   2. No"," ❯ 2. No");
        assert_eq!(channel_card(&r,Some(&moved),&identity).unwrap()["question_hash"],full["question_hash"]);
        // No chooser, an unreadable terminal, or another request's complete panel.
        for screen in [DENIED,"",BASH] {assert!(channel_card(&r,Some(screen),&identity).is_none(),"{screen}");}
        assert!(channel_card(&r,None,&identity).is_none());
        // A child's request may be the clipped one; a matching panel is not.
        let mut child=r.clone();
        questions::observe(&mut child,&json!({"hook_event_name":"PermissionRequest","agent_id":"child","tool_name":"Read","tool_input":{"file_path":PATH}}));
        assert!(channel_card(&child,Some(clipped),&identity).is_none());
        assert!(channel_card(&child,Some(SAFETY),&identity).is_some());
        let mut paused=r.clone();paused["phase"]=json!("working");
        assert!(channel_card(&paused,Some(clipped),&identity).is_none());
    }
    #[test] fn waiting_hook_card_refuses_a_panel_a_child_may_own() {
        let command="printf hi > out.txt";
        let mut r=hooked("Bash",json!({"command":command,"description":"Write \"hi\" into out.txt without a trailing newline"}));
        let identity=channel::identity(&r).unwrap();
        assert!(channel_card(&r,Some(BASH),&identity).is_some());
        child(&mut r,"child-a",command);
        assert!(channel_card(&r,Some(BASH),&identity).is_none());
        assert!(channel_snapshot(&r,BASH).is_none(),"no keys for an ambiguous panel either");
        assert_eq!(card(&r,BASH)["can_answer"],false,"the screen card stays ambiguous");
    }
    #[test] fn waiting_hook_card_hash_covers_the_warning_but_not_its_countdown() {
        let command="T=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port";
        let r=hooked("Bash",json!({"command":command}));
        let identity=channel::identity(&r).unwrap();
        let clipped=&SAFETY[SAFETY.find(" │ T=$(pwd)").unwrap()..];
        let hash=|screen:&str|channel_card(&r,Some(screen),&identity).unwrap()["question_hash"].clone();
        assert_eq!(hash(clipped),hash(&clipped.replace("in 2:00,","in 1:59,")));
        assert_ne!(hash(clipped),hash(&clipped.replace("possibly-empty","possibly-wrong")));
    }
    // Two Reads of the same file name in different folders: Claude shows the
    // first ("1 of 2") while the hook waits for the second (Claude 2.1.296).
    fn read_panel(path:&str)->String {
        format!("────────────────────────────────────────\n Read file                                   1 of 2\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Read({path})\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, allow reading from {} during this session\n   3. No\n\n Esc to cancel · Tab to amend\n",
            Path::new(path).parent().unwrap().display())
    }
    #[test] fn a_hook_never_grants_one_request_under_another_requests_panel() {
        let (a,b)=("/work/outsideA/notes.txt","/work/outsideB/notes.txt");
        let r=hooked("Read",json!({"file_path":b}));
        let identity=channel::identity(&r).unwrap();
        let shown_a=read_panel(a);
        // The legacy match accepts A's panel for B by file name.
        assert!(matches(&r["pending_approval"],&parse(&shown_a).unwrap()));
        assert!(channel_card(&r,Some(&shown_a),&identity).is_none(),"no hook card under A's panel");
        // The screen path answers the panel it shows, with its own keys.
        let screen=card(&r,&shown_a);
        assert_eq!(screen["answer_transport"],"claude_tui");
        assert!(string(&screen["questions"][0],"body").contains(a));
        // Even B's own complete Read panel proves only what Read shows; the hook
        // card appears only for a panel the screen cannot answer, with B in full.
        assert!(channel_card(&r,Some(&read_panel(b)),&identity).is_none());
        let clipped=read_panel(a).split_once(" Read file").unwrap().1.split_once("\n").unwrap().1.split_once("\n").unwrap().1.to_owned();
        assert!(parse(&clipped).is_none());
        let q=channel_card(&r,Some(&clipped),&identity).unwrap();
        let body=string(&q["questions"][0],"body");
        assert!(body.contains(b)&&!body.contains(a),"{body}");
        assert_eq!((q["panel_verified"].clone(),q["tool_call_id"].clone()),(json!(false),json!("toolu_one")));
        assert_eq!(options(&q),[("Yes".into(),"once".into()),("No".into(),"once".into())],"no broader option of another request");
        assert!(channel_snapshot(&r,&clipped).is_none(),"no keys fallback without a proven panel");
    }
    #[test] fn only_the_whole_bash_operation_proves_a_panel() {
        let base=json!({"command":"printf hi > out.txt","description":"Write \"hi\" into out.txt without a trailing newline"});
        let r=hooked("Bash",base.clone());
        assert!(proves(&r["pending_approval"],&parse(BASH).unwrap()));
        // The same command with another field the panel does not show.
        for (key,value) in [("dangerouslyDisableSandbox",json!(true)),("run_in_background",json!(true)),("timeout",json!(600000))] {
            let mut input=base.clone();input[key]=value.clone();
            let r=hooked("Bash",input);
            assert!(!proves(&r["pending_approval"],&parse(BASH).unwrap()),"{key}");
            let identity=channel::identity(&r).unwrap();
            assert!(channel_card(&r,Some(BASH),&identity).is_none(),"{key}: left to the screen path");
            let clipped=BASH.split_once(" Bash command\n").unwrap().1;
            let q=channel_card(&r,Some(clipped),&identity).unwrap();
            assert!(string(&q["questions"][0],"body").contains(&format!("\"{key}\": {value}")),"{key}: disclosed");
            assert_eq!(q["panel_verified"],false);
        }
        // Another description, or a child's panel with the same command.
        let mut other=base.clone();other["description"]=json!("Something else");
        assert!(!proves(&hooked("Bash",other)["pending_approval"],&parse(BASH).unwrap()));
        let child_panel=BASH.replace(" Bash command\n"," Bash command · from the \"fixture-checks\" workflow\n");
        assert!(!proves(&r["pending_approval"],&parse(&child_panel).unwrap()));
        // Another active Bash call may own the panel: no proof, no hook card.
        let identity=channel::identity(&r).unwrap();
        assert!(channel_card(&r,Some(BASH),&identity).unwrap()["panel_verified"]==true);
        let mut two=r.clone();
        channel::observe(&mut two,&json!({"hook_event_name":"PreToolUse","tool_use_id":"toolu_other","tool_name":"Bash","tool_input":{"command":"printf  hi > out.txt"}}));
        assert!(channel_card(&two,Some(BASH),&identity).is_none());
    }
    #[test] fn a_fetch_of_the_same_host_is_not_the_same_operation() {
        let r=hooked("WebFetch",json!({"url":"https://example.com/private/report","prompt":"title?"}));
        let identity=channel::identity(&r).unwrap();
        assert!(matches(&r["pending_approval"],&parse(FETCH).unwrap()),"the legacy match checks the host only");
        assert!(channel_card(&r,Some(FETCH),&identity).is_none());
        let clipped=FETCH.split_once(" Fetch\n").unwrap().1;
        let q=channel_card(&r,Some(clipped),&identity).unwrap();
        assert!(string(&q["questions"][0],"body").contains("https://example.com/private/report"));
        assert_eq!((q["panel_verified"].clone(),options(&q).len()),(json!(false),2));
    }
    #[test] fn a_warning_is_shown_only_for_the_command_it_belongs_to() {
        let command="T=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port";
        let clipped=&SAFETY[SAFETY.find(" │ T=$(pwd)").unwrap()..];
        let own=hooked("Bash",json!({"command":command}));
        let q=channel_card(&own,Some(clipped),&channel::identity(&own).unwrap()).unwrap();
        assert!(string(&q["questions"][0],"body").contains("Dangerous rm operation"));
        assert_eq!(string(&q["questions"][0],"body").matches("T=$(pwd)").count(),1,"the warning rows only, not the command again");
        // Another command's warning is never shown as this one's: no hook card.
        let other=hooked("Bash",json!({"command":"rm -f build/*.o"}));
        assert!(channel_card(&other,Some(clipped),&channel::identity(&other).unwrap()).is_none());
        let only_warning=&clipped[clipped.find("╌").unwrap()..];
        assert!(channel_card(&own,Some(only_warning),&channel::identity(&own).unwrap()).is_none(),"no visible command, no attribution");
        // A blank row of a multi-line command keeps only its bare gutter.
        let script="printf one\n\nT=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port";
        let multi=hooked("Bash",json!({"command":script}));
        let blank=clipped.replace(" │ T=$(pwd)"," │ printf one\n │\n │ T=$(pwd)");
        let q=channel_card(&multi,Some(&blank),&channel::identity(&multi).unwrap()).unwrap();
        assert!(string(&q["questions"][0],"body").contains("Dangerous rm operation"));
        // A complete safety panel of the same command proves it, and adds its warning once.
        let full=channel_card(&own,Some(SAFETY),&channel::identity(&own).unwrap()).unwrap();
        assert_eq!(full["panel_verified"],true);
        assert_eq!(string(&full["questions"][0],"body").matches("T=$(pwd)").count(),1);
        assert!(string(&full["questions"][0],"body").contains("Dangerous rm operation"));
    }

    #[test] fn any_diagnostic_the_card_cannot_attribute_leaves_it_to_the_screen() {
        let command="T=$(pwd); test \"$T\" = /workspace/example && rm -f $T/*.jsonl $T/*.port";
        let own=hooked("Bash",json!({"command":command}));
        let identity=channel::identity(&own).unwrap();
        let clipped=&SAFETY[SAFETY.find(" │ T=$(pwd)").unwrap()..];
        assert!(channel_card(&own,Some(clipped),&identity).is_some());
        // Another diagnostic than the dangerous-rm one, clipped: no hook card.
        let flag=clipped.replace("│ Dangerous rm operation on possibly-empty variable path:","│ Dangerous flag combination detected:");
        assert!(diagnostic(&flag)&&safety_panel(&flag).is_none());
        assert!(channel_card(&own,Some(&flag),&identity).is_none());
        // A proven panel whose diagnostic comes with other choices: no hook card.
        let choices=SAFETY.replace("   2. No","   2. Yes, and always allow\n   3. No");
        assert!(diagnostic(&choices)&&safety_panel(&choices).is_none());
        assert!(proves(&own["pending_approval"],&parse(&choices).unwrap()));
        assert!(channel_card(&own,Some(&choices),&identity).is_none());
        // An ordinary panel shows no diagnostic.
        assert!(!diagnostic(BASH)&&!diagnostic(OUTSIDE));
        // A clipped warning is this command's only while no other Bash call is
        // active, and only when the visible rows end this command.
        let mut two=own.clone();
        channel::observe(&mut two,&json!({"hook_event_name":"PreToolUse","tool_use_id":"toolu_other","tool_name":"Bash","tool_input":{"command":"ls"}}));
        assert!(channel_card(&two,Some(clipped),&identity).is_none());
        let longer=hooked("Bash",json!({"command":format!("{command} && printf more")}));
        assert!(channel_card(&longer,Some(clipped),&channel::identity(&longer).unwrap()).is_none(),"the visible rows do not end this command");
    }

    #[test] fn waiting_hook_card_keeps_broader_options_behind_the_screen() {
        let r=hooked("Bash",json!({"command":"printf hi > out.txt","description":"Write \"hi\" into out.txt without a trailing newline"}));
        let q=channel_card(&r,Some(BASH),&channel::identity(&r).unwrap()).unwrap();
        assert_eq!(options(&q),options(&card(&r,BASH)));
        assert_eq!(q["panel_verified"],true);
        assert_eq!(q["questions"][0]["body"],"Bash\n\nprintf hi > out.txt\n\nWrite \"hi\" into out.txt without a trailing newline",
            "the hook's request itself, in full");
        // Every field of a non-Bash request is shown when its panel is not.
        let write=hooked("Write",json!({"file_path":"/work/project/new.txt","content":"secret\nline"}));
        let body=request_body(&write["pending_approval"]).unwrap();
        assert!(body.starts_with("Write\n\n{")&&body.contains("\"content\": \"secret\\nline\""),"{body}");
        let flagged=request_body(&json!({"tool_name":"Bash","tool_input":{"command":"ls","dangerouslyDisableSandbox":true}})).unwrap();
        assert!(flagged.contains("\"dangerouslyDisableSandbox\": true"),"{flagged}");
        assert!(request_body(&json!({"tool_name":"Bash","tool_input":{"command":"x".repeat(16_001)}})).is_none());
        assert!(request_body(&json!({"tool_name":"Bash","tool_input":{"command":" "}})).is_none());
    }

    #[test] fn native_options_are_verbatim_and_broader_ones_are_marked() {
        let bash=card(&record("Bash",json!({"command":"printf hi > out.txt","description":"Write \"hi\" into out.txt without a trailing newline"})),BASH);
        assert_eq!(bash["can_answer"],true);assert_eq!(bash["tool_call_id"],"toolu_one");assert_eq!(bash["created_at"],10.0);
        assert_eq!(bash["questions"][0]["question"],"Do you want to proceed?");
        assert!(string(&bash["questions"][0],"body").starts_with("Bash command\n\n"));
        assert!(string(&bash["questions"][0],"body").ends_with("without a trailing newline\n\nprintf hi > out.txt"));
        assert_eq!(options(&bash),[("Yes","once"),("Yes, and always allow access to /work/project from this project","broader"),
            ("Yes, and switch to auto mode · auto mode handles these prompts for you","broader"),("No","once")].map(|(l,s)|(l.to_owned(),s.to_owned())));
        let outside=card(&record("Read",json!({"file_path":PATH})),OUTSIDE);
        assert_eq!(outside["can_answer"],true);assert_eq!(outside["questions"][0]["question"],"Allow this read outside the working directories?");
        assert_eq!(options(&outside).iter().map(|(_,s)|s.as_str()).collect::<Vec<_>>(),["broader","broader","once","once"]);
        let fetch=card(&record("WebFetch",json!({"url":"https://example.com","prompt":"title?"})),FETCH);
        assert_eq!(fetch["can_answer"],true);assert_eq!(options(&fetch)[2].1,"once");
        let write=card(&record("Write",json!({"file_path":"/work/project/new.txt","content":"x"})),WRITE);
        assert_eq!(write["can_answer"],true);assert_eq!(options(&write)[1].1,"broader");
        assert_eq!(parse(OUTSIDE).unwrap().selected,0);
        assert_eq!(parse(&BASH.replace(" ❯ 1. Yes","   1. Yes").replace("   4. No"," ❯ 4. No")).unwrap().selected,3);
    }

    /// Real captured panels: the card's header carries the hook's tool name,
    /// which names the automatic-approval category (zerus-sz42x).
    #[test] fn automatic_approval_categories_read_the_hook_tool_name() {
        use zerus_client_core::approvals::automatic::{categories,once,Category};
        for (tool,input,screen,expected) in [
            ("Bash",json!({"command":"printf hi > out.txt","description":"Write \"hi\" into out.txt without a trailing newline"}),BASH,Category::Shell),
            ("Read",json!({"file_path":PATH}),OUTSIDE,Category::Read),
            ("WebFetch",json!({"url":"https://example.com","prompt":"title?"}),FETCH,Category::Web),
            ("Write",json!({"file_path":"/work/project/new.txt","content":"x"}),WRITE,Category::Edit),
        ] {
            let card=card(&record(tool,input),screen);
            assert!(once("claude",&card).is_some(),"{tool}");
            assert_eq!(categories::tool("claude",&card).category,expected,"{tool}");
        }
    }

    #[test] fn mismatched_or_unreadable_panels_stay_visible_for_terminal() {
        let other=card(&record("Read",json!({"file_path":"/home/user/other.png"})),OUTSIDE);
        assert_eq!(other["can_answer"],false);assert!(options(&other).is_empty());
        assert_eq!(other["questions"][0]["body"],"Read\n\n/home/user/other.png");
        let bash=record("Bash",json!({"command":"rm changed"}));
        assert_eq!(card(&bash,BASH)["can_answer"],false);
        // The top of a tall panel scrolled away: the chooser remains visible.
        let clipped=OUTSIDE.split_once("Read outside the working directories\n").unwrap().1;
        assert!(parse(clipped).is_none());assert_eq!(card(&record("Read",json!({"file_path":PATH})),clipped)["can_answer"],false);
        let unread=build(&record("Read",json!({"file_path":PATH})),None).unwrap().0;
        assert_eq!(unread["can_answer"],false);assert_eq!(unread["questions"][0]["question"],"Claude asks to use Read");
        let plan=card(&record("ExitPlanMode",json!({"plan":"Ship it"})),FETCH);
        assert_eq!(plan["answer_unavailable_reason"],"Review this plan in Terminal.");
    }

    #[test] fn resolved_question_and_unrelated_choosers_hide_the_card() {
        let read=record("Read",json!({"file_path":PATH}));
        assert!(build(&read,Some(DENIED)).is_none());assert!(acknowledged(DENIED));assert!(!acknowledged(OUTSIDE));
        let mut asked=record("AskUserQuestion",json!({"questions":[]}));assert!(build(&asked,Some(OUTSIDE)).is_none());
        asked["pending_approval"]["tool_name"]=json!("Read");asked["phase"]=json!("input");assert!(build(&asked,Some(OUTSIDE)).is_none());
        // Extra or malformed rows below the options are not this chooser.
        assert!(parse(&OUTSIDE.replace("Esc to cancel · Tab to amend","Status line")).is_none());
        assert!(parse(&OUTSIDE.replace("   4. Yes, but ask again next time","   5. Yes, but ask again next time")).is_none());
        assert!(parse(&OUTSIDE.replace("   3. No, and","❯  3. No, and")).is_none());
        assert!(parse(&OUTSIDE.replace("\n\n Esc to cancel","\n Esc to cancel")).is_none());
    }

    #[test] fn claude_code_questions_without_a_tool_call_point_to_terminal() {
        // Claude Code asks this itself after a safeguard stop: no PreToolUse,
        // no active tool, only the permission prompt of AskUserQuestion.
        let input=json!({"metadata":{"source":"refusal_fallback_preference"},"questions":[{"header":"Model switch",
            "multiSelect":false,"question":"Switch models and keep going whenever this happens?","options":[
            {"label":"Switch automatically","description":"Continue on the other model now"},
            {"label":"Stay","description":"Stop here without switching"}]}]});
        let mut asked=record("AskUserQuestion",input);asked["phase"]=json!("input");asked["active_tools"]=json!({});
        let card=native_question(&asked,None).unwrap();
        assert_eq!((card["source"].clone(),card["can_answer"].clone(),card["created_at"].clone()),
            (json!("claude_native_question"),json!(false),json!(10.0)));
        assert_eq!(card["answer_unavailable_reason"],"Claude Code asks this itself. Answer it in Terminal.");
        let item=&card["questions"][0];
        assert_eq!((item["header"].clone(),item["allow_other"].clone()),(json!("Model switch"),json!(false)));
        assert_eq!(item["question"],"Switch models and keep going whenever this happens?");
        assert_eq!(item["options"][0]["label"],"Switch automatically");
        assert_eq!(item["options"][1]["description"],"Stop here without switching");
        // A request Zerus cannot normalize still shows that Claude waits.
        let mut odd=asked.clone();odd["pending_approval"]["tool_input"]=json!({"questions":"?"});
        let odd=native_question(&odd,None).unwrap();
        assert_eq!(odd["questions"][0]["question"],"Claude asks a question in Terminal");
        assert!(odd["questions"][0]["options"].as_array().unwrap().is_empty());
        assert_ne!(odd["question_hash"],card["question_hash"]);
        // The model's own question keeps claude_question's answerable card.
        let mut model=asked.clone();model["active_tools"]=json!({"toolu_ask":{"name":"AskUserQuestion"}});
        assert!(native_question(&model,None).is_none());
        let mut resolved=asked.clone();resolved["phase"]=json!("working");assert!(native_question(&resolved,None).is_none());
        let mut tool=asked.clone();tool["pending_approval"]["tool_name"]=json!("Read");assert!(native_question(&tool,None).is_none());
        assert!(build(&asked,None).is_none());
    }

    // Claude Code 2.1.296 at 92 columns, below assistant output.
    const MODEL_SWITCH:&str="⏺ Checking the panel:\n\n────────────────────────────────────────────────────────────\n ☐ Model switch\n\n│ Switch models and keep going whenever this happens?\n\n❯ 1. Switch automatically\n     Continue on the other model now\n  2. Stay\n     Stop here without switching\n  3. Type something.\n────────────────────────────────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n\n✻ Waiting for API response · will retry in 0s · check your network\n";
    const PAUSED:&str="⏺ Opus 5.5's safeguards stopped the response above · continuing once with that noted\n\n────────────────────────────────────────────────────────────\n Session paused\n\n  Opus 5.5's safeguards flagged this session. Edit and retry, or\n  continue with Opus 4.8.\n\n  Details: `[cyber]`\n\n  ❯ 1. Switch to Opus 4.8\n    2. Edit prompt and retry with Opus 5.5\n\n✻ Waiting for API response · will retry in 2m 40s · check your network\n";
    const IDLE:&str="⏺ Done.\n\n────────────────────────────────────────────────────────────\n❯ 1. first draft item\n  2. second draft item\n────────────────────────────────────────────────────────────\n  ⏵⏵ bypass permissions on\n";

    #[test] fn claude_code_questions_follow_the_terminal() {
        let input=json!({"questions":[{"header":"Model switch","question":"Switch models and keep going whenever this happens?",
            "options":[{"label":"Switch automatically","description":"Continue on the other model now"},
            {"label":"Stay","description":"Stop here without switching"}]}]});
        let mut asked=record("AskUserQuestion",input);asked["phase"]=json!("input");asked["active_tools"]=json!({});
        assert!(native_question(&asked,Some(MODEL_SWITCH)).is_some());
        // Answered in Terminal, then another chooser or the composer.
        assert!(native_question(&asked,Some(PAUSED)).is_none());
        assert!(native_question(&asked,Some(IDLE)).is_none());
        assert!(native_question(&asked,Some(&MODEL_SWITCH.replace("  2. Stay\n","\n  2. Stay\n"))).is_some());
    }

    #[test] fn other_claude_terminal_choosers_point_to_terminal() {
        let mut waiting=json!({"agent":"claude","phase":"input","run_id":"run","conversation_id":"conversation",
            "attention_started":20.0,"active_tools":{}});
        let card=terminal_choice(&waiting,PAUSED).unwrap();
        assert_eq!((card["source"].clone(),card["can_answer"].clone(),card["created_at"].clone()),
            (json!("claude_terminal_choice"),json!(false),json!(20.0)));
        let item=&card["questions"][0];
        assert_eq!(item["question"],"Session paused");
        assert_eq!(item["body"],"Opus 5.5's safeguards flagged this session. Edit and retry, or\ncontinue with Opus 4.8.\n\nDetails: `[cyber]`");
        assert_eq!(item["options"].as_array().unwrap().iter().map(|o|string(o,"label")).collect::<Vec<_>>(),
            ["Switch to Opus 4.8","Edit prompt and retry with Opus 5.5"]);
        // Moving the highlight keeps the same card.
        let moved=PAUSED.replace("  ❯ 1. Switch","    1. Switch").replace("    2. Edit","  ❯ 2. Edit");
        assert_eq!(terminal_choice(&waiting,&moved).unwrap()["question_hash"],card["question_hash"]);
        let question=terminal_choice(&waiting,MODEL_SWITCH).unwrap();
        assert_eq!(question["questions"][0]["question"],"☐ Model switch");
        assert_eq!(question["questions"][0]["body"],"Switch models and keep going whenever this happens?");
        assert_eq!(question["questions"][0]["options"][1]["description"],"Stop here without switching");
        assert_eq!(question["questions"][0]["options"].as_array().unwrap().len(),3);
        // A composer draft is not a chooser, and only waiting sessions are read.
        assert!(terminal_choice(&waiting,IDLE).is_none());
        assert!(terminal_choice(&waiting,&PAUSED.replace("  ❯ 1.","    1.")).is_none());
        waiting["phase"]=json!("working");assert!(terminal_choice(&waiting,PAUSED).is_none());
    }

    #[test] fn wrapped_commands_tips_and_footers_still_match_the_request() {
        // Claude 2.1.296 fills the row with a token longer than its width and
        // continues it on the next row without a space.
        let long="cat /work/example/projects/a-really-long-directory-name/contracts/template-with-a-long-name.html";
        let bash=record("Bash",json!({"command":long,"description":"Show the template"}));
        let split=BASH.replace(" printf hi > out.txt\n╌",
            " cat /work/example/projects/a-really-long-directory-name/contrac\n ts/template-with-a-long-name.html\n╌");
        assert_eq!(card(&bash,&split)["can_answer"],true);
        for changed in [split.replace("contrac\n","contra\n"),split.replace(" cat /work"," cut /work"),
            split.replace("\n ts/","\n ts /")] {
            assert_eq!(card(&bash,&changed)["can_answer"],false,"{changed}");
        }
        // Claude 2.1.284 draws the tip, command and description unfenced.
        let hi=record("Bash",json!({"command":"printf hi > out.txt","description":"Write \"hi\" into out.txt without a trailing newline"}));
        let unfenced="────────────────────────────────────────\n Bash command\n Tip: auto mode handles these prompts for you — choose \"switch to\n auto mode\" below\n printf hi > out.txt\n Write \"hi\" into out.txt without a\n trailing newline\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel · Tab to\n amend\n";
        assert_eq!(card(&hi,unfenced)["can_answer"],true);
        assert_eq!(card(&hi,&unfenced.replace(" auto mode\" below\n",""))["can_answer"],false);
        assert_eq!(card(&hi,&unfenced.replace("printf hi","printf ho"))["can_answer"],false);
        assert!(parse(&OUTSIDE.replace(" Esc to cancel · Tab to amend"," Esc to cancel · Tab to\n amend")).is_some());
        assert!(parse(&OUTSIDE.replace("\n\n Esc to cancel · Tab to amend","\n Esc to cancel · Tab to\n amend")).is_none());
    }

    #[test] fn windows_normal_screen_picker_is_a_bottom_chooser() {
        // Claude Code 2.1.296 under bundled ConPTY (spike S3) keeps the
        // normal-screen layout: the /model picker below a full-width rule.
        let screen=include_str!("../../tests/fixtures/claude-model-picker-windows.txt");
        let (title,_,options)=bottom_choice(screen).unwrap();
        assert_eq!((title.as_str(),options.len()),("Select model",5));
        assert!(chooser_visible(screen)&&parse(screen).is_none());
    }

    #[test] fn any_bottom_chooser_counts_as_visible_for_dismissal() {
        for screen in [MODEL_SWITCH,PAUSED,BASH,OUTSIDE] {assert!(chooser_visible(screen),"{screen}");}
        for screen in [IDLE,DENIED,""] {assert!(!chooser_visible(screen),"{screen}");}
    }

    // Claude Code 2.1.296 after an approval answered in Terminal: No or Esc
    // with the composer (120 columns) or the one-time auto-mode nudge (100
    // columns) below; Yes before and while the command runs (80 columns).
    const ANSWERED_NO:&str=include_str!("../../tests/fixtures/claude-approval-denied.txt");
    const ANSWERED_NO_NUDGE:&str=include_str!("../../tests/fixtures/claude-approval-denied-nudge.txt");
    const ANSWERED_YES:&str=include_str!("../../tests/fixtures/claude-approval-starting.txt");
    const RUNNING:&str=include_str!("../../tests/fixtures/claude-approval-running.txt");
    // The same Claude Code in a later turn, after an earlier denial (100 columns).
    const RUNNING_TIP:&str=include_str!("../../tests/fixtures/claude-approval-running-tip.txt");
    const LONG:&str="sleep 6 && printf %s fixture-output-with-a-rather-long-name-to-wrap-the-row > /work/example/projects/a-really-long-directory-name/approved.txt\necho second line";
    // The same Claude Code while the request streams, before its panel.
    const STREAMING:&str="❯ Use the tool now.\n\n\n· Sublimating…\n                                                  ● high · /effort\n────────────────────────────────────────────────────────────\n❯\u{a0}\n────────────────────────────────────────────────────────────\n  ⏸ manual mode on · esc to interrupt · ← for agents\n";

    #[test] fn terminal_denial_is_the_interruption_that_ends_the_transcript() {
        let bash=record("Bash",json!({"command":LONG}));
        for screen in [ANSWERED_NO,ANSWERED_NO_NUDGE,DENIED] {assert_eq!(answered(&bash,screen),Some(false),"{screen}");}
        // The collapsed row no longer names the tool; the ended turn is enough.
        assert_eq!(answered(&record("Read",json!({"file_path":PATH})),ANSWERED_NO),Some(false));
        let summary="✻ Crunched for 0s · done 7:47 PM";
        assert!(ANSWERED_NO.contains(summary));
        assert_eq!(answered(&bash,&ANSWERED_NO.replace(summary,"✻ Sautéed for 1m 3s · done 7:48 PM")),Some(false));
        // A newer turn below an earlier interruption: its prompt, tool or spinner.
        for later in ["❯ Try again","● Bash(ls)","✢ Actualizing… (0s · ↓ 52 tokens)","Ordinary output"] {
            assert_eq!(answered(&bash,&ANSWERED_NO.replace(summary,&format!("{summary}\n\n{later}"))),None,"{later}");
        }
        let earlier=format!("  ⎿  {INTERRUPTED}\n\n✻ Cooked for 0s · done 7:40 PM\n\n{STREAMING}");
        assert_eq!(answered(&bash,&earlier),None);
        // Another interruption text, or no rule closing the transcript.
        assert_eq!(answered(&bash,&ANSWERED_NO.replace(" · What should Claude do instead?","")),None);
        let cut=ANSWERED_NO.split_once(summary).unwrap().0;
        assert_eq!(answered(&bash,cut),None);
        // A numbered chooser below the ended turn still waits for an answer.
        let numbered=ANSWERED_NO_NUDGE.replace("❯ Yes, set","❯ 1. Yes, set").replace("  No, keep","  2. No, keep");
        assert_eq!((denied(&numbered),answered(&bash,&numbered)),(true,None));
    }

    #[test] fn terminal_approval_is_the_requested_command_running() {
        let bash=record("Bash",json!({"command":LONG,"description":"Wait and write"}));
        assert_eq!(answered(&bash,RUNNING),Some(true));
        assert!(RUNNING.contains("echo second line (3s)\n     (ctrl+b to run in background)\n"));
        for (from,to) in [("(3s)","(1m 4s · 5 lines)"),(" (3s)","\n     (3s)"),("     (ctrl+b to run in background)\n","")] {
            assert_eq!(answered(&bash,&RUNNING.replace(from,to)),Some(true),"{to}");
        }
        // Before the command starts or after it ends, no elapsed time proves it.
        assert_eq!(answered(&bash,ANSWERED_YES),None);
        assert_eq!(answered(&bash,&RUNNING.replace(" (3s)","").replace("     (ctrl+b to run in background)\n","")),None);
        assert_eq!(answered(&bash,STREAMING),None);
        // Another command, another tool, or anything newer below the command.
        let mut other=bash.clone();other["pending_approval"]["tool_input"]["command"]=json!(LONG.replace("second","third"));
        assert_eq!(answered(&other,RUNNING),None);
        let mut fetch=bash.clone();fetch["pending_approval"]["tool_name"]=json!("WebFetch");
        assert_eq!(answered(&fetch,RUNNING),None);
        assert_eq!(answered(&bash,&RUNNING.replace("(ctrl+b to run in background)","(ctrl+b to run in background)\n● Read(file)")),None);
        // From the second turn on, Claude draws a tip under its spinner. The
        // earlier interruption above the newer prompt is no denial either.
        let again=record("Bash",json!({"command":"sleep 4; printf approved >> approved.txt"}));
        assert!(RUNNING_TIP.contains("✽ Crunching… (3s · ↓ 24 tokens)\n  ⎿ \u{a0}Tip: Start with small features"));
        assert!(!denied(RUNNING_TIP));
        assert_eq!(answered(&again,RUNNING_TIP),Some(true));
        assert_eq!(answered(&again,&RUNNING_TIP.replace(" (3s)\n","\n")),None);
        // Only indented rows belong to the spinner; a row at the margin is newer.
        assert_eq!(answered(&again,&RUNNING_TIP.replace("  ⎿ \u{a0}Tip:","● Tip:")),None);
        assert_eq!(answered(&again,&RUNNING_TIP.replace("     suggested edits","suggested edits")),None);
    }

    #[test] fn only_a_waiting_request_reads_the_screen_to_settle() {
        let bash=record("Bash",json!({"command":LONG}));
        assert!(pending(&bash));
        assert!(pending(&record("Read",json!({"file_path":PATH}))));
        // Another phase or agent, a failed or pausing run, a question, no request.
        let mut others=Vec::new();
        for (key,value) in [("phase",json!("tool")),("phase",json!("interrupted")),("phase",json!("input")),("agent",json!("codex")),
            ("error",json!("Provider failed")),("pausing",json!({"pid":std::process::id(),"start":process_start(std::process::id())}))] {
            let mut changed=bash.clone();changed[key]=value;others.push(changed);
        }
        for tool in ["AskUserQuestion",""] {
            let mut changed=bash.clone();changed["pending_approval"]["tool_name"]=json!(tool);others.push(changed);
        }
        let mut settled=bash.clone();settled.as_object_mut().unwrap().remove("pending_approval");others.push(settled);
        for other in others {
            assert!(!pending(&other),"{other}");
            // Reconciling returns such a record as it was, without a terminal.
            assert_eq!(reconcile(other.clone()),other);
        }
    }

    #[test] fn hash_ignores_the_highlight_but_not_the_request() {
        let read=record("Read",json!({"file_path":PATH}));
        let moved=OUTSIDE.replace(" ❯ 1. Yes, and keep","   1. Yes, and keep").replace("   2. No, and block"," ❯ 2. No, and block");
        assert_eq!(snapshot(&read,OUTSIDE).unwrap().0,snapshot(&read,&moved).unwrap().0);
        assert_eq!(snapshot(&read,&moved).unwrap().1,1);
        let mut other=read.clone();other["pid"]=json!(2);
        assert_ne!(snapshot(&read,OUTSIDE).unwrap().0,snapshot(&other,OUTSIDE).unwrap().0);
    }
}
