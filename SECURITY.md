# Security policy

Stockroom downloads programs and puts them on your computer, so a way to make it install
the wrong thing, or write where it should not, matters to us. Thank you for reporting such
things responsibly.

## Reporting a vulnerability

Please **do not open a public issue**. Report it privately through
[GitHub's private vulnerability reporting](https://github.com/amazynk/stockroom/security/advisories/new),
or by email to [help@amazynk.com](mailto:help@amazynk.com) if you can't use GitHub.

Include the version or the commit you built from, your operating system, what an attacker
could achieve, and the steps that reproduce it. We will acknowledge the report within a few
days, keep you updated while we fix it, and credit you in the advisory unless you prefer
otherwise.

## Scope

In scope: a download that is used although its size or SHA-256 does not match the app list;
an archive or disk image that makes Stockroom write outside the folder it unpacks into; an
app list that gets past the format's rules (unsafe IDs or paths, download addresses that are
not HTTPS, an older list accepted over a newer one); anything that makes Stockroom run with
more rights than the user who started it, or change another user's files; and crashes or
hangs from a crafted list, archive or state file.

Out of scope: problems that need an attacker who already runs code as your user, and what
an installed app does once it runs. The app list is not signed: Stockroom takes what
amazynk.com serves over HTTPS, and the README says so.

## Supported versions

There is no release yet. Until 1.0, only the latest release gets security fixes.
