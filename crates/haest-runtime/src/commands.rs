//! [`Runtime::command`]: a slash command the conversation's mods or skills
//! answer.

use haest_core::skills::load_skill_body;
use haest_mods::CommandOutcome;

use crate::{Runtime, Ui};

impl<U: Ui + 'static> Runtime<U> {
    /// `/name args` as the conversation answers it, for a host whose own
    /// commands did not: a mod's command first ([`Mods::command`], the later
    /// mod's when two answer the name), then a skill of that name, whose body
    /// with `args` goes to the model ([`CommandOutcome::Prompt`]).
    /// [`CommandOutcome::Unknown`] when neither has the name: a host sends
    /// the line as typed. `name` is taken with or without its `/`.
    ///
    /// A skill whose file cannot be read is said to the [`Ui`] as an error,
    /// and is [`CommandOutcome::Handled`]: nothing goes to the model.
    ///
    /// [`Mods::command`]: haest_mods::Mods::command
    pub async fn command(&self, name: &str, args: &str) -> CommandOutcome {
        let name = name.strip_prefix('/').unwrap_or(name);
        match self.mods.command(name, args).await {
            CommandOutcome::Unknown => {}
            outcome => return outcome,
        }
        let Some(skill) = self.skills.list().into_iter().find(|s| s.name == name) else {
            return CommandOutcome::Unknown;
        };
        match load_skill_body(self.fs.as_ref(), &skill, Some(args)).await {
            Ok(body) => CommandOutcome::Prompt(body),
            Err(e) => {
                self.hooks
                    .error(&format!("Skill {name} could not be read: {e}"));
                CommandOutcome::Handled
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use haest_core::{
        fs::{Fs, MemFs},
        skills::SkillSources,
    };
    use haest_mods::CommandOutcome;
    use haest_settings::Settings;

    use crate::{
        BuildOptions, RuntimeInputs, World, build,
        testkit::{Deny, Recorded, settings},
    };

    const SKILL: &str = "/w/.claude/skills/tidy/SKILL.md";

    /// A mod's command, then a skill, then nothing: what each is to the host.
    #[tokio::test]
    async fn a_command_is_a_mods_then_a_skills_then_unknown() {
        let skill = "---\nname: tidy\ndescription: Tidy up.\n---\nTidy the code.";
        let fs = Arc::new(MemFs::from_files([(SKILL, skill)]));
        let settings = Settings {
            skills: SkillSources {
                claude: true,
                ..SkillSources::NONE
            },
            ..settings()
        };
        let inputs = RuntimeInputs::new(settings, World::files(fs.clone()))
            .with_keys(|k: &str| (k == "OPENROUTER_API_KEY").then(|| "k".to_string()));
        let rt = build(inputs, Recorded::default(), Deny, BuildOptions::default())
            .await
            .unwrap();

        assert_eq!(rt.command("plan", "off").await, CommandOutcome::Handled);
        let tidy = rt.command("/tidy", "src/a.rs").await;
        assert!(
            matches!(&tidy, CommandOutcome::Prompt(body) if body.contains("Tidy the code.") && body.contains("src/a.rs")),
            "{tidy:?}"
        );
        assert_eq!(rt.command("nope", "").await, CommandOutcome::Unknown);

        // A skill whose file went away is said, and nothing goes to the model.
        fs.remove(std::path::Path::new(SKILL)).await.unwrap();
        assert_eq!(rt.command("tidy", "").await, CommandOutcome::Handled);
        assert_eq!(rt.ui().warnings().len(), 1, "{:?}", rt.ui().warnings());
    }
}
