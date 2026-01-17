<!-- omit in toc -->

# Contributing to stevelr/anytype

First off, thanks for taking the time to contribute! ❤️ All types of contributions are encouraged and valued.

> If you like the project, but don't have time to contribute right now, that's fine. There are other easy ways to support the project and show your appreciation:
>
> - Star the [repo](https://github.com/stevelr/anytype)
> - Post a link online
> - Refer this project in your project's readme
> - Tell your friends and colleagues!

<!-- omit in toc -->

## Table of Contents

- [Documentation](#documentation)
- [I Have a Question](#i-have-a-question)
- [I Want To Contribute](#i-want-to-contribute)
  - [Reporting Bugs](#reporting-bugs)
  - [Submitting PRs](#submitting-prs)
  - [Suggesting Enhancements](#suggesting-enhancements)

## Documentation

- **anytype** (client library):
  - [README.md](./anytype-api/README.md)
  - [docs.rs](https://docs.rs/anytype)
  - [Troubleshooting](./anytype-api/Troubleshooting.md) guide with a list of known issues and debugging tips
- **anyr**:
  - [README.md](./anyr/README.md)
  - `anyr --help`
- **any-edit**:
  - [README.md](./any-edit/README.md)
  - `any-edit --help`

## I Have a Question

Questions are welcome!

> First, please make sure you checked the available [Documentation](#documentation), and looked at open [Issues](https://github.com/stevelr/anytype/issues) that might be related. If you find an existing issue that still need clarification, you can add your question to that issue.

To submmit a question,

- Open an [Issue](https://github.com/stevelr/anytype/issues/new).
- Provide as much context as you can about what you're running into.
- List the version you are using, the OS and platform, and anything else you think is relevant

We will then take care of the issue as soon as possible.

## I Want To Contribute

> ### Legal Notice <!-- omit in toc -->
>
> When contributing to this project, you must agree that you have authored 100% of the content, that you have the necessary rights to the content and that the content you contribute may be provided under the project licence.

### Reporting Bugs

<!-- omit in toc -->

#### Before Submitting a Bug Report

A good bug report shouldn't leave others needing to chase you up for more information. Therefore, we ask you to investigate carefully, collect information and describe the issue in detail in your report. Please complete the following steps in advance to help us fix any potential bug as fast as possible.

- Make sure that you are using the latest version.
- Make sure that you have read the [documentation](#documentation). If you are looking for support, see [asking questions](#i-have-a-question)).
- Run with RUST_LOG=debug and see if the error messages provide additional useful information
- To see if other users have experienced (and potentially already solved) the same issue you are having, check if there is not already a bug report existing for your bug or error in the [bug tracker](https://github.com/stevelr/anytype/issues?q=label%3Abug).
- Collect information about the bug:
  - If the issue is UI-related, take screenshots or screen recordings
  - Stack trace (Traceback), if available
  - Error messages, program output, or logs (preferably, output with RUST_LOG=debug)
  - OS, Platform and Version (Windows, Linux, macOS, x86, ARM)
  - Version of the program or libraries used
  - command-line flags - how the program was run
  - Can you reliably reproduce the issue? And can you also reproduce it with older versions?
  - Anything else about the environment, or what you were doing before the bug occurred

<!-- omit in toc -->

#### How Do I Submit a Good Bug Report?

> Do not report security related issues, vulnerabilities or bugs including sensitive information to the issue tracker, or elsewhere in public. Instead sensitive bugs must be sent by email to <anytype-rust@pm.me>.

<!-- You may add a PGP key to allow the messages to be sent encrypted as well. -->

We use GitHub issues to track bugs and errors. If you run into an issue with the project:

- Open an [Issue](https://github.com/stevelr/anytype/issues/new).
- Explain the behavior you would expect and the actual behavior.
- Please provide as much context as possible and describe _steps to reproduce the issue_. This part is important. If we aren't able to reproduce the problem, it may be harder for us to understand and fix it.
- Provide the information you collected in the previous section.

Once it's filed:

- We'll review the issue and try to reproduce it.
- We may ask follow-up questions

### Submitting PRs

PRs are encouraged!

Before submitting a PR,

- Make sure the PR is based off the latest `main` branch.
- Follow the code style of the project. Make sure it passes checks in `just check`

### Suggesting Enhancements

This section guides you through submitting a suggestion for projects in this repository, whether they are new features or minor improvements to existing functionality.

<!-- omit in toc -->

#### Before Submitting an Enhancement

- Make sure that you are using the latest version.
- Read the [documentation](#documentation) carefully and find out if the functionality is already covered
- Check [issues](https://github.com/stevelr/anytype/issues) and [PRs](https://github.com/stevelr/anytype/pulls) to see if the enhancement has already been suggested or is in the works. If it has, add a thumbs-up on that issue or PR to show your support. If your use case or requirements for the feature are different, add a comment

<!-- omit in toc -->

#### How Do I Submit a Good Enhancement Suggestion?

Enhancement suggestions are tracked as [GitHub issues](https://github.com/stevelr/anytype/issues).

- Use a **clear and descriptive title** for the issue to identify the suggestion.
- Provide a **step-by-step description of the suggested enhancement** in as many details as possible.
- **Describe the current behavior** and **explain which behavior you expected to see instead** and why. At this point you can also tell which alternatives do not work for you.
- You may want to **include screenshots or screen recordings** which help you demonstrate the steps or point out the part which the suggestion is related to. You can use [LICEcap](https://www.cockos.com/licecap/) to record GIFs on macOS and Windows, and the built-in [screen recorder in GNOME](https://help.gnome.org/users/gnome-help/stable/screen-shot-record.html.en) or [SimpleScreenRecorder](https://github.com/MaartenBaert/ssr) on Linux. <!-- this should only be included if the project has a GUI -->
- **Explain why this enhancement would be useful** to most stevelr/anytype users. You may also want to point out the other projects that solved it better and which could serve as inspiration.

<!-- You might want to create an issue template for enhancement suggestions that can be used as a guide and that defines the structure of the information to be included. If you do so, reference it here in the description. -->
