# Translating PuppetCat

PuppetCat's interface is built in Slint, whose `@tr()` mechanism produces
standard gettext `.po` files, the format translators already know how to work
with. The interface strings are not wrapped in `@tr()` yet, so there is nothing
to translate today. When that lands, this file will say where the `.po` files
live, how to regenerate the template, and how a translation is reviewed before
it ships.
