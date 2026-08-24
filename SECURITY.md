# Security Policy

Anytype Toolbox accesses data from Anytype, a knowledge management app and service that many people choose
because of its local-first privacy-preserving technology and policies.
We take security seriously. We want to know about any vulnerabilities and pledge that we will investigate
all credible reported security issues.

## Please report privately

**Do not open a public issue, discussion, or pull request for a suspected
vulnerability, and do not create a public repo with a reproducible test case.**
Public disclosure before a fix is available puts every user at risk.

Report privately:

- **GitHub private vulnerability reporting**
  Go to the repository's **Security** tab → **Report a vulnerability**. This
  opens a private advisory only you and the maintainers can see.

You can help by making the report clear, and, ideally, including a reproducible
test case demonstrating the vulnerability. If it looks like it's AI generated,
or is too long, verbose, or unclear, we may not take it seriously. If we can't
reproduce it, it will take longer to investigate, and that's not in either of our
interests.

Please include:

- the version or commit (`anyr --version`)
  - did you use a signed binary or custom build.
- the version of the Anytype desktop app or headless cli you were using
- OS platform and version
- a description of the issue and its impact,
- reproduction steps or a proof of concept,
- any suggested remediation.
- your contact information

**Redact real secrets, keys, or tokens** from anything you attach.

## What to expect

There is **no bug bounty** and no formal SLA. That said:

- We are grateful for feedback and getting a heads-up for security bugs.
- We aim to acknowledge a credible report within **2 business days**.
- We will keep you updated as we investigate and fix.
- We practice coordinated disclosure: we will agree on a disclosure timeline
  with you and, unless you prefer otherwise, credit you in the release notes and
  advisory.

## Understanding the trust model and risks

Before reporting, it helps to know what Anytype Toolbox and does **not** defend against.

If you intend to grant an llm access to Anytype vault data, it is strongly recommended to use a separate space:

- Create a space (using the desktop app) that you intend to share with the LLM
- Install the anytype headless cli,
- Authenticate with the cli, not the desktop app, and invite it to the llm-shared space

With your desktop app, you can easily move between spaces and access all of them, while the LLM can only access spaces for which it received a join invite code. **This keeps your personal data separate and significantly reduces all the risks below.**

**Access Credentials**

- `anyr` records vault credentials in the OS Keystore, or for headless/server installations,
  an sqlite db file on disk. You can control the location of the db file. The 'accountKey' is the most
  sensitive credential. Someone who has that key can download an entire vault.

- After `anyr` is authenticated, it remains authenticated until you use `anyr auth logout`. Someone with command-line access to your machine could use it to read or modify vault data. If credentials are in the OS Keyring, access is often limited by timeouts and screen lock controls.

**Data Privacy and Integrity**

- `anyr` can export data from Anytype through many commands: `search`, `get`, `backup`, etc.
  You choose what data to export and where it is stored.

- Communication between `anyr` and `Anytype` app or headless cli is on a localhost (`127.0.0.1`) connection, but is not encrypted. A process on your machine may be able to intercept that traffic and observe or record credentials and data.

- `anyr` can modify data in an Anytype vault.

### Additional Risks for LLM use

- **Data Exfiltration**

If you allow an llm agent or harness to use `anyr`, including `anyr mcp`, there are additional risks to data privacy:

- If the agent has network access, your data *could* be sent over the network by the agent.
- If the agent model is cloud-hosted (for example, Anthropic, OpenAI, or Grok models), you are allowing the models to access the data.

- **Prompt Injection**

The SKILL.md files instruct llms to ignore instructions in content, but llms may not always follow that guidance. Use only on content you trust.
