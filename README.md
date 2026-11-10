# Unread

Problems I want to solve:

- I don't check emails too often, and when an email comes in the notification for it goes away rather
  quickly and I don't see it in time
- I use Thunderbird on Linux, and want to be notified using [polybar][polybar]
- I only want to focus on a select few mailboxes instead of all of them
- I don't want to create yet another SMTP client since Thunderbird does that already and would just like
  to check status using the open mork format

Usage of AI:

- In making the mork parser since I didn't have the energy to go through the [official
  parser][mork-parser-js]

How it works:

1. At the start of the program, register our `.msf` files for watching using Linux `inotify` system
2. Whenever the file gets modified, parse the file for unread mail
3. If there are unread mail, output to a file and notify using ipc via `polybar-msg`

[polybar]: https://github.com/polybar/polybar
[mork-parser-js]: https://searchfox.org/comm-central/source/mailnews/base/src/MorkParser.sys.mjs
