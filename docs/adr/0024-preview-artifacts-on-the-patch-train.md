# 0024 — Preview artifacts ship on the patch train

- Status: accepted
- Date: 2026-08-08
- Supersedes: [0019](0019-version-ladder-and-release-trains.md)'s platform
  rows, its "no new platform" rule for patch releases, and its two statements
  that a minor release *adds* a platform (its Decision rule on minor releases,
  and its "macOS lands as `v0.2.0`, Linux as `v0.3.0`" consequence). Under this
  ADR a minor **confirms** a platform rather than adding one. 0019's `v1.0.0`
  row stands unchanged.

## Context

[ADR-0019](0019-version-ladder-and-release-trains.md) mapped each platform onto
its own minor: `v0.2.0` = "the macOS app assembly ships", `v0.3.0` = "the Linux
port ships". It also ruled that patch releases on the `v0.1.x` Windows train
carry "fixes and low-risk improvements only — **no new platform**".

Three phases closed against that ladder and none of them released. `m6-macos`,
`m7-linux` and `m8-hardening` are all tagged; `v0.2.0`, `v0.3.0` and `v1.0.0`
are all held, on one condition between them: **nobody has run either build on
the hardware it targets.** That was recorded as a decision rather than a
blocker, and it was the right call each time it was made.

Two things about that state are worth stating plainly, because together they
are why this ADR exists.

**The hold was self-blocking.**
[ADR-0013](0013-macos-ddc-wrap-vs-vendor.md) keeps the macOS DDC path labelled
experimental until there are "at least three independent community
confirmations per architecture". [D-014](../debt.md#d-014) says the same, and
the P6 gate demonstrated what a hardware-blind codec is worth: a fully green
test suite described a wire no display could answer, because purity buys
host-testability and not correctness against an external protocol. So the exit
from experimental runs through *other people's hardware* — and the artifact
those people would run has never been on the Releases page. The condition for
releasing was community confirmation; the mechanism for getting community
confirmation was releasing. Holding forever was the default outcome of that
loop, and three closed phases with nothing shipped is what it looks like.

**The hold had no mechanism.** It was never enforced by anything in the
repository. `release.yml` has no version-line gating whatsoever — verified by
reading every `if:` in the file — so the `macos` and `linux` jobs run on any
`v*` tag and their artifacts are folded into the same `SHA256SUMS`, the same
minisign pass, the same provenance attestation and the same Release. The only
thing implementing "held" was a human not pushing a tag. That is worth knowing
before choosing between "keep holding" and "ship with a label": one of those
was already one command away from happening by accident.

## Decision

**The macOS disk image and the Linux tarball ship on the `v0.1.x` train,
beginning with `v0.1.6`, labelled as unverified previews.**

1. A `v0.1.x` release publishes all four artifacts. The Windows installer and
   portable zip are what they have always been. The `.dmg` and the `.tar.gz`
   are **previews**: built, staged, checksummed, signed and attested by the
   same pipeline, and **never run on the hardware they target**.

2. `v0.2.0` and `v0.3.0` are **re-mapped**. They no longer mean "the port first
   ships". They mean **the platform has been confirmed on real hardware** —
   for macOS, ADR-0013's threshold of three independent community confirmations
   per architecture; for Linux, a human running the tray on a real desktop
   session per [qa-checklist.md](../qa-checklist.md). A version that means
   "confirmed" is a claim this project can keep. One that means "shipped" was
   already spent.

3. ADR-0019's **"no new platform" rule for patch releases is superseded to the
   extent of preview artifacts, and no further.** A schema break, an IPC
   protocol bump, or promoting a platform out of preview still requires a
   minor. The rule's purpose was that a `v0.1.x` upgrade must never surprise a
   Windows user, and shipping additional files that a Windows user does not
   download preserves that purpose exactly.

4. **A release carrying an unconfirmed platform must say so in its own release
   notes**, and that is a committed file rather than a memory:
   [`docs/release-notes-preamble.md`](../release-notes-preamble.md) is prepended
   to the git-cliff output by `release.yml`. A label applied by whoever happens
   to be cutting the release is the
   [false-assurance](../plan.md#how-work-lands) shape this project has a rule
   against. When a platform leaves preview, the preamble is edited in the same
   PR that bumps the version.

   **Why not `cliff.toml`'s header**, stated precisely because the first version
   of this ADR got it wrong and a reviewer disproved it by running the command.
   The claim was that `--strip all` removes the header and footer so the config
   cannot carry this. That is not a constraint - `--strip` is chosen on the same
   command line, `--strip footer` demonstrably keeps the header, and `cliff.toml`
   has no footer configured at all. The real reason is narrower and is about
   ownership: `cliff.toml`'s `[changelog] header` is **`CHANGELOG.md`'s** header
   ("# Changelog / All notable changes to Duja are documented here..."), which is
   the wrong text for a release body and the right text for the file it belongs
   to. One key cannot be both. A separate file is the cheaper answer than a
   second changelog config.

## Consequences

- **A macOS or Linux user can now install code that has never executed on their
  hardware.** That is the cost, stated without softening. The mitigations are
  real, partial, and **uneven across the two platforms** - the first draft of
  this list said "gamma and overlay state is crash-guarded (`duja --restore`,
  crash-marker recovery)" as though that held everywhere, and a reviewer caught
  that it does not:

  | | X11 | Wayland | macOS |
  |---|---|---|---|
  | `duja --restore` | resets every CRTC | nothing to do, and says so | the only route |
  | automatic crash recovery | crash marker | not needed - the ramp dies with the process | **none: no marker is written** |
  | `dujactl doctor` session report | yes | yes | display info only |

  So the platform with the weakest recovery story is the one whose report says
  least, and neither `doctor` nor anything else detects a missing
  `StatusNotifierItem` host. The preamble states this per-platform rather than
  in one reassuring sentence. None of it substitutes for the run, which is why
  the artifacts are labelled rather than announced.

- **The update checker's reach is unchanged *today*, and will not stay that
  way.** It prompts on a newer stable release via `/releases/latest`, and every
  installed copy of Duja is currently a Windows one, so this decision adds no
  prompt that did not already exist. But `updates.rs` carries no platform `cfg`:
  once a Mac or Linux user installs a preview, the *next* release prompts them,
  and it prompts them toward another preview. That is a new consequence of this
  decision rather than a pre-existing one, and it is the strongest argument for
  keeping the preamble accurate - it is what a returning preview user sees.

- **`v1.0.0` stays held, and its condition is unchanged.** ADR-0019 defines it
  as including "cross-platform hardware sign-off", and a preview is the opposite
  of a sign-off. What this decision changes is that the sign-off is now
  *obtainable*: previews are the instrument that produces it.

- **Four documents stop being true the moment this ships** and are corrected in
  the same release, in the `v0.1.6` docs PR rather than this one:

  | file | what goes stale |
  |---|---|
  | `README.md` | "There is no macOS or Linux download yet" (`:131`), "**Linux (x64).** No release yet" (`:78`), and the absence of any macOS install section |
  | `SECURITY.md` | "Two of the four have never been published" (`:99`) |
  | `docs/STATUS.md` | the two `held` release rows |
  | `docs/plan.md` | the ladder section, the phase table's `v0.2.0 held` / `v0.3.0 held`, and the "held rather than pending" list |

  The list said *three* until a reviewer pointed out that `plan.md` - the
  entry-point doc - asserts the old ladder in four places. `docs/adr/0019`'s own
  file is deliberately untouched: `docs/adr/README.md` says a superseded
  decision's text stays and the *index row* carries the annotation.

  The support matrix's 🧪 cells stay 🧪 — "written and CI-tested, never run on
  real hardware" is still precisely what they are, and a download link does not
  change it.

- **What replaces the hold is the label**, and a label is weaker than a gate.
  This is accepted deliberately, with the preamble file as the mechanism that
  keeps it from depending on anybody's memory. If a future release ever ships a
  preview artifact without the preamble naming it, that is a defect of the same
  class as a budget row naming an instrument that does not exist.
