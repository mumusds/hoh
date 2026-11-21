//! Seed legacy test descriptions as Markdown plus the real generated index (REQ-034).

use hoh_core::specindex::{self, Source};
use serde_json::Value;
use std::path::Path;

pub fn write(
    project: &Path,
    spec: &Value,
    default_check: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut markdown =
        "# 用户\nFixture\n# 范围\nFixture\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n"
            .to_owned();
    for requirement in spec["requirements"].as_array().ok_or("no requirements")? {
        let id = requirement["id"].as_str().ok_or("no requirement id")?;
        let title = requirement["title"]
            .as_str()
            .unwrap_or("Fixture requirement");
        markdown.push_str(&format!("## {id} {title}\n"));
        for ac in requirement["acceptance"]
            .as_array()
            .ok_or("no acceptance")?
        {
            let id = ac["id"].as_str().ok_or("no AC id")?;
            let body = ac["body"].as_str().unwrap_or("fixture result");
            let check = ac["check_id"].as_str().unwrap_or(default_check);
            markdown.push_str(&format!(
                "### {id}\ngiven: fixture\nwhen: checked\nthen: {body}\ncheck: {check}\n"
            ));
        }
    }
    let source = Source {
        path: "docs/spec/fixture.md".into(),
        bytes: markdown.into_bytes(),
    };
    let index = specindex::generate_index(std::slice::from_ref(&source))?;
    std::fs::create_dir_all(project.join("docs/spec"))?;
    std::fs::write(project.join(&source.path), &source.bytes)?;
    std::fs::write(project.join(specindex::PATH), index.bytes())?;
    Ok(String::from_utf8(index.bytes().to_vec())?)
}
