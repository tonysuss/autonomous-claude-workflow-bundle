"""Writes the S1 probe skills and agent into the S1 repository."""
import os

R = os.path.join(os.path.dirname(os.path.abspath(__file__)), "s1repo")
FILES = {
    ".github/skills/s1-model/SKILL.md": "---\nname: s1-model\ndescription: S1 probe, plain model-invoked skill. Use when asked for the s1 model probe.\n---\n\nMARKER-MODEL: the body of the plain skill.\n",
    ".github/skills/s1-user/SKILL.md": "---\nname: s1-user\ndescription: S1 probe, a skill marked disable-model-invocation. Use when asked for the s1 user probe.\ndisable-model-invocation: true\n---\n\nMARKER-USER: the body of the flagged skill.\n",
    ".github/skills/s1-hidden/SKILL.md": "---\nname: s1-hidden\ndescription: S1 probe, a skill marked user-invocable false. Use when asked for the s1 hidden probe.\nuser-invocable: false\n---\n\nMARKER-HIDDEN: the body of the hidden skill.\n",
    ".github/skills/s1-router/SKILL.md": "---\nname: s1-router\ndescription: S1 probe, a router skill with routed references. Use when asked for the s1 router probe.\n---\n\nMARKER-ROUTER. For the routed material, read references/routed.md next to this file.\n",
    ".github/skills/s1-router/references/routed.md": "MARKER-ROUTED: routed reference content.\n",
    ".github/agents/s1-agent.agent.md": "---\nname: s1-agent\ndescription: S1 probe custom agent standing in for a user-invoked skill.\ntools: [\"read\", \"search\", \"execute\"]\n---\n\nMARKER-AGENT: you are the s1 probe agent.\n",
    "README.md": "s1 probe repository\n",
}
for rel, text in FILES.items():
    path = os.path.join(R, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)
print("\n".join(sorted(FILES)))
