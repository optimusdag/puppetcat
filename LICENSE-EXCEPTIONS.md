# PuppetCat licensing exceptions

PuppetCat is licensed under the **GNU Affero General Public License, version 3 or
later** (AGPL-3.0-or-later). The full text is in [`LICENSE`](LICENSE).

The additional permission below is granted by the copyright holders on top of
that license. It grants rights; it never takes any away. If you ignore this
file entirely, plain AGPL-3.0-or-later applies and you are still compliant.

<!-- This file is carried over from Concat, the project PuppetCat was forked
     from, under the "you may extend this exception to your version" note
     below. It has been reworded to name PuppetCat throughout; nothing in
     the grant's substance has changed. -->

---

## The PuppetCat Plugin Exception, version 1.0

### Why this exists

The **PuppetCat API** is a stable interface that lets plugins, scripts, automation
and MCP integrations extend the editor without forking it.

Without this exception, a plugin that links against the PuppetCat API would form a
combined work with PuppetCat, and the AGPL would reach the plugin's own source.
That is not the intent. The copyleft is here to keep *PuppetCat itself* — the
engine, the compositor, the renderer, the editor — open. It is not here to
dictate the license of software that merely talks to it.

### The grant

As a special exception, the copyright holders of PuppetCat give you permission to
combine an Independent Module with PuppetCat to produce a combined work, and to
convey the resulting combined work, **without the Independent Module itself
becoming subject to the requirements of the AGPL**, under the conditions below.

An **Independent Module** is a module that:

1. is not derived from, and does not incorporate any part of, PuppetCat's source
   code; and
2. interfaces with PuppetCat exclusively through the PuppetCat API, its plugin
   entry points, its command/IPC surface, or its documented file and project
   formats.

### Conditions

This exception applies only while all of the following hold.

1. **PuppetCat stays AGPL.** Every portion of PuppetCat in the combined work — the
   original source and any modification you make to it — remains licensed
   under AGPL-3.0-or-later, and you meet the AGPL's obligations for it,
   including the source-offer requirement of section 13 when the combined work
   is made available to users over a network.
2. **The API is the boundary.** The Independent Module reaches PuppetCat only
   through the interfaces named above. Statically or dynamically linking
   against PuppetCat's internal crates, vendoring its source, or reproducing a
   substantial portion of its code in your module places that module outside
   this exception.
3. **No relabelling.** This exception does not permit conveying PuppetCat, a
   modified PuppetCat, or a substantial portion of PuppetCat under terms other than
   the AGPL by describing it as a plugin, a host, a runtime, or a bundled
   dependency of an Independent Module.
4. **Trademarks are separate.** This exception grants no rights in the PuppetCat
   name or logo. See [`TRADEMARK.md`](TRADEMARK.md).

### Notes

If you modify PuppetCat, you may extend this exception to your version, but you
are not obliged to. If you do not wish to do so, delete this file from your
version.

Nothing here limits the permissions the AGPL already grants you, and nothing
here is a licence to any patent beyond what AGPL-3.0-or-later section 11
already provides.

---

## Commercial licensing

The AGPL's reciprocity is deliberate: if you build on PuppetCat's engine, the
result stays open for the people who use it.

If that does not fit your situation, and PuppetCat's contributor copyright is
held centrally (all contributions are made under the CLA in [`CLA.md`](CLA.md)),
a separate commercial licence can be granted without the AGPL's obligations.
Enquiries: **optimusdag@gmail.com**.

<!-- This offer only holds while PuppetCat actually operates a CLA that
     centralises contributor copyright (see CLA.md). If contributions are
     ever accepted without one, this paragraph should be removed or scoped
     to code the maintainer wrote outright. -->
