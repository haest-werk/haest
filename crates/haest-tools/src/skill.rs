//! The Skill tool: the model's door into progressive disclosure. The system
//! prompt lists name + description; calling this returns the full SKILL.md
//! body with its base directory so relative references resolve.

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    fs::Fs,
    skills::{DiskSkills, Skill, SkillSet},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_str, req_str},
};
use serde_json::{Value, json};

/// What the Skill tool reads.
#[derive(Clone)]
pub struct SkillToolOptions {
    /// The skills, asked on every call.
    set: Arc<dyn SkillSet>,
    /// The parameter lists the offered names (`enum`): a weak model can't misspell one.
    name_enum: bool,
}

impl SkillToolOptions {
    /// SKILL.md files: `skills` is the current list, the bodies are read from
    /// `host_fs` (a user skill lives outside any sandbox mount).
    pub fn new(
        skills: impl Fn() -> Vec<Skill> + Send + Sync + 'static,
        host_fs: Arc<dyn Fs>,
    ) -> Self {
        Self::over(Arc::new(DiskSkills::new(skills, host_fs)))
    }

    /// A host's own skills.
    pub fn over(set: Arc<dyn SkillSet>) -> Self {
        Self {
            set,
            name_enum: false,
        }
    }

    /// Name the offered skills in the parameter's `enum`.
    pub fn with_name_enum(mut self) -> Self {
        self.name_enum = true;
        self
    }
}

struct SkillTool(SkillToolOptions);

pub fn create_skill_tool(opts: SkillToolOptions) -> Arc<dyn Tool> {
    Arc::new(SkillTool(opts))
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
        "Load a skill by name (your instructions list the skills). Call it before you start a task a skill is for; it returns the skill's instructions: follow them.".into()
    }
    fn parameters(&self) -> Value {
        let mut skill = json!({"type": "string", "description": "The skill name"});
        if self.0.name_enum {
            let names: Vec<String> = self.0.set.offered().into_iter().map(|c| c.name).collect();
            skill["enum"] = json!(names);
        }
        json!({
            "type": "object",
            "properties": {
                "skill": skill,
                "args": {"type": "string", "description": "Optional arguments for the skill"}
            },
            "required": ["skill"]
        })
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = req_str(&input, "skill")?.trim();
        let asked = raw.strip_prefix('/').unwrap_or(raw);
        let offered: Vec<String> = self.0.set.offered().into_iter().map(|c| c.name).collect();
        // Exact first; else the one name that differs only in case.
        let name = offered.iter().find(|n| *n == asked).or_else(|| {
            let mut alike = offered.iter().filter(|n| n.eq_ignore_ascii_case(asked));
            alike.next().filter(|_| alike.next().is_none())
        });
        let Some(name) = name else {
            let known = if offered.is_empty() {
                "(none)".to_string()
            } else {
                offered.join(", ")
            };
            return Err(ToolError::Input(format!(
                "No skill named \"{asked}\". Available: {known}"
            )));
        };
        let body = self.0.set.load(name, opt_str(&input, "args")?).await?;
        // Read whole: a skill's instructions spilled to a file would go unread.
        Ok(ToolOutput {
            sized: true,
            ..ToolOutput::text(body)
        })
    }
}

#[cfg(test)]
mod tests {
    use haest_core::{
        fs::MemFs,
        skills::{SkillOpts, SkillSources, discover_skills},
    };

    use super::*;
    use crate::testkit::{input, shut_ctx};

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
            sources: SkillSources::ALL,
        };
        let skills = discover_skills(fs.as_ref(), &opts).await;
        create_skill_tool(SkillToolOptions::new(move || skills.clone(), fs))
    }

    #[tokio::test]
    async fn a_listed_skill_loads_its_body_with_or_without_a_slash() {
        let t = tool().await;
        let ctx = shut_ctx();
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
        let ctx = shut_ctx();
        let hidden = t.execute(input(json!({"skill": "secret"})), &ctx).await;
        assert!(matches!(hidden, Err(ToolError::Input(_))));
        let res = t.execute(input(json!({"skill": "nope"})), &ctx).await;
        let Err(ToolError::Input(msg)) = res else {
            panic!("an input error expected")
        };
        // The refusal names what the model may call, and only that.
        assert!(msg.contains("deploy") && !msg.contains("secret"));
    }

    #[tokio::test]
    async fn a_hosts_own_skills_load_through_the_tool_named_by_an_enum() {
        use haest_core::skills::{InlineSkill, InlineSkills, SkillCard};
        let notes = InlineSkill {
            card: SkillCard {
                name: "notes".into(),
                description: "Mine.".into(),
                when: String::new(),
                origin: Some("personal".into()),
            },
            body: "My way.".into(),
        };
        let set = Arc::new(InlineSkills::merged([vec![notes]]));
        let t = create_skill_tool(SkillToolOptions::over(set.clone()).with_name_enum());
        assert_eq!(
            t.parameters()["properties"]["skill"]["enum"],
            json!(["notes"])
        );
        let plain = create_skill_tool(SkillToolOptions::over(set));
        assert!(
            plain.parameters()["properties"]["skill"]
                .get("enum")
                .is_none()
        );
        let out = t
            .execute(input(json!({"skill": " Notes "})), &shut_ctx())
            .await
            .unwrap();
        assert_eq!(out.content, "Skill notes (personal): Mine.\n\nMy way.");
        assert!(out.sized, "a skill is read whole, never spilled to a file");
    }
}
