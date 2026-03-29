# Neomacs vs GNU Emacs Module Audits

**Date**: 2026-03-28

This directory now has one audit file per major compatibility module.

- [Phase 1: Lisp VM Core](phase-01-lisp-vm-core.md)
- [Phase 2: Buffer & Text](phase-02-buffer-and-text.md)
- [Phase 3: I18n / Character / Coding](phase-03-i18n-character-coding.md)
- [Phase 4: Search / Read / Print / File I/O](phase-04-search-read-print-file-io.md)
- [Phase 5: Editing Commands](phase-05-editing-commands.md)
- [Phase 6: Window / Frame / Font / Terminal](phase-06-window-frame-font-terminal.md)
- [Phase 7: Display Engine](phase-07-display-engine.md)
- [Phase 8: Command System](phase-08-command-system.md)
- [Phase 8 Keymap/Key Input Plan](phase-08-keymap-key-input-refactor-plan.md)
- [Phase 9: Process / Thread / Timer](phase-09-process-thread-timer.md)
- [Phase 10: Startup & Integration](phase-10-startup-integration.md)

Existing overview files remain important:

- [Compatibility Audit Sequence](neomacs-gnu-compatibility-audit-sequence.md)
- [Bootstrap Pipeline](bootstrap-pipeline-gnu-vs-neomacs.md)
- [GNU Backend Coupling Map](gnu-emacs-backend-module-dependencies.md)

These audits are not a claim that Neomacs already matches GNU Emacs. They are
the current audit result and the required direction to make Neomacs 100%
semantically identical to GNU Emacs.

Each phase file is intended to stay source-code-level:

- which GNU source files own the semantics
- which Neomacs source files own them today
- where Neomacs ownership is split or architecturally wrong
- what the long-term ideal ownership should be
- what exit criteria would justify calling the phase GNU-compatible

Important scope note:

- Phase 1 is about GNU-compatible Lisp-visible semantics, not copying GNU
  Emacs's internal VM architecture.
- Phases 2 and above should stay much closer to GNU ownership and behavior,
  because that is where "load the same GNU Lisp and preserve the same editor
  semantics" matters most.
