# 0024 — Preview artifacts ship on the patch train

- Status: accepted
- Date: 2026-08-08
- Supersedes: the platform rows and the "no new platform" rule of
  [0019](0019-version-ladder-and-release-trains.md)

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
   to the git-cliff output by `release.yml`. `git cliff --strip all` removes the
   configured header and footer, so `cliff.toml` cannot carry this; and a label
   applied by whoever happens to be cutting the release is the
   [false-assurance](../plan.md#how-work-lands) shape this project has a rule
   against. When a platform leaves preview, the preamble is edited in the same
   PR that bumps the version.

## Consequences

- **A macOS or Linux user can now install code that has never executed on their
  hardware.** That is the cost, stated without softening. The mitigations are
  real but partial: gamma and overlay state is crash-guarded
  (`duja --restore`, crash-marker recovery), `dujactl doctor` reports what a
  session can actually do before the tray is launched, and the Linux gamma path
  refuses rather than silently doing nothing on a transport it cannot drive.
  None of that is a substitute for the run, which is exactly why the artifacts
  are labelled rather than announced.

- **The update checker's reach is unchanged.** It prompts on a newer stable
  release via GitHub's `/releases/latest`, and today every installed copy of
  Duja is a Windows one. The ports have no installed base to notify, so this
  decision adds no prompt that did not already exist.

- **`v1.0.0` stays held, and its condition is unchanged.** ADR-0019 defines it
  as including "cross-platform hardware sign-off", and a preview is the opposite
  of a sign-off. What this decision changes is that the sign-off is now
  *obtainable*: previews are the instrument that produces it.

- **Three documents stop being true the moment this ships** and are corrected in
  the same release: `README.md`'s "There is no macOS or Linux download yet" and
  its support-matrix note, `SECURITY.md`'s "Two of the four have never been
  published", and `docs/STATUS.md`'s two `held` release rows. The support
  matrix's 🧪 cells stay 🧪 — "written and CI-tested, never run on real
  hardware" is still precisely what they are, and a download link does not
  change it.

- **What replaces the hold is the label**, and a label is weaker than a gate.
  This is accepted deliberately, with the preamble file as the mechanism that
  keeps it from depending on anybody's memory. If a future release ever ships a
  preview artifact without the preamble naming it, that is a defect of the same
  class as a budget row naming an instrument that does not exist.
