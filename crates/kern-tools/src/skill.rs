//! The Skill tool: the model's door into progressive disclosure. The system
//! prompt lists name + description; calling this returns the full SKILL.md
//! body with its base directory so relative references resolve.

use std::sync::Arc;

use async_trait::async_trait;
use kern_core::{
    fs::Fs,
    skills::{Skill, load_skill_body},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_str, req_str},
};
use serde_json::{Value, json};

type SkillList = Box<dyn Fn() -> Vec<Skill> + Send + Sync>;

struct SkillTool {
    skills: SkillList,
    host_fs: Arc<dyn Fs>,
}

/// `skills`: the current list, asked on every call. `host_fs`: the body is the
/// host's reading -- a user skill lives outside any sandbox mount.
pub fn create_skill_tool(
    skills: impl Fn() -> Vec<Skill> + Send + Sync + 'static,
    host_fs: Arc<dyn Fs>,
) -> Arc<dyn Tool> {
    Arc::new(SkillTool {
        skills: Box::new(skills),
        host_fs,
    })
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }
    fn kind(&self) -> &str {
        "skill.invoke"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "Load a skill by name. Returns its full instructions; follow them. Only names listed under \"# Skills\" in the system prompt are valid.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "skill": {"type": "string", "description": "The skill name"},
                "args": {"type": "string", "description": "Optional arguments for the skill"}
            },
            "required": ["skill"]
        })
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = req_str(&input, "skill")?;
        let name = raw.strip_prefix('/').unwrap_or(raw);
        let offered: Vec<Skill> = (self.skills)()
            .into_iter()
            .filter(|s| s.model_invocable)
            .collect();
        let Some(skill) = offered.iter().find(|s| s.name == name) else {
            let known: Vec<&str> = offered.iter().map(|s| s.name.as_str()).collect();
            let known = if known.is_empty() {
                "(none)".to_string()
            } else {
                known.join(", ")
            };
            return Err(ToolError::Input(format!(
                "No skill named \"{name}\". Available: {known}"
            )));
        };
        let body = load_skill_body(self.host_fs.as_ref(), skill, opt_str(&input, "args")?).await?;
        Ok(ToolOutput::text(body))
    }
}

#[cfg(test)]
mod tests {
    use kern_core::{
        fs::MemFs,
        skills::{SkillOpts, discover_skills},
        workspace::ShutWorkspace,
    };

    use super::*;

    async fn tool() -> Arc<dyn Tool> {
        let fs: Arc<dyn Fs> = Arc::new(MemFs::with_files([
            (
                "/h/.claude/skills/deploy/SKILL.md",
                "---\nname: deploy\ndescription: ships it\n---\n\nDEPLOY BODY\n",
            ),
            (
                "/h/.claude/skills/secret/SKILL.md",
                "---\nname: secret\ndescription: x\ndisable-model-invocation: true\n---\n\nHIDDEN\n",
            ),
        ]));
        let opts = SkillOpts {
            cwd: "/w".into(),
            home: "/h".into(),
            config_dir: "/h/.claude".into(),
        };
        let skills = discover_skills(fs.as_ref(), &opts).await;
        create_skill_tool(move || skills.clone(), fs)
    }

    fn input(v: Value) -> JsonMap {
        v.as_object().cloned().unwrap()
    }

    #[tokio::test]
    async fn a_listed_skill_loads_its_body_with_or_without_a_slash() {
        let t = tool().await;
        let ctx = ToolContext::new("/w", Arc::new(ShutWorkspace));
        for name in ["deploy", "/deploy"] {
            let out = t
                .execute(input(json!({"skill": name, "args": "prod"})), &ctx)
                .await
                .unwrap();
            assert!(out.content.contains("DEPLOY BODY") && out.content.contains("prod"));
        }
    }

    #[tokio::test]
    async fn a_skill_the_model_may_not_invoke_is_refused_like_an_unknown_one() {
        let t = tool().await;
        let ctx = ToolContext::new("/w", Arc::new(ShutWorkspace));
        let hidden = t.execute(input(json!({"skill": "secret"})), &ctx).await;
        assert!(matches!(hidden, Err(ToolError::Input(_))));
        let res = t.execute(input(json!({"skill": "nope"})), &ctx).await;
        let Err(ToolError::Input(msg)) = res else {
            panic!("an input error expected")
        };
        // The refusal names what the model may call, and only that.
        assert!(msg.contains("deploy") && !msg.contains("secret"));
    }
}
