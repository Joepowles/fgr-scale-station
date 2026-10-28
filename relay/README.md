# Bug report relay

A Cloudflare Worker between the app's Bug report tab and GitHub. The app
posts the report here; the Worker opens the issue with a GitHub token kept
as a Worker secret, so no token ships in the installer.

First deploy, from this folder:

    wrangler login
    wrangler deploy
    wrangler secret put GITHUB_TOKEN      # paste a fine-grained token: Issues read/write on the one repository

The Worker's address (printed by `wrangler deploy`) is the default `support.relayUrl` in `src/main/settings.js`.

Rotating the token later is `wrangler secret put GITHUB_TOKEN` again; the app does not change.

## Sample pictures

The relay also takes sample pictures from a station with "Send sample
pictures" ticked on its Bug report tab (src/samples.js): one .tar per truck
visit, with its frames, log and visit.json. They wait in the
`scale-station-samples` KV namespace for 30 days, and uploads are only taken
through the day in `SAMPLES_UNTIL` in wrangler.toml. Reading them needs a key:

    wrangler secret put SAMPLES_KEY       # any long random string

Then, on the dev machine:

    SAMPLES_KEY=... node relay/fetch-samples.mjs yard-samples

downloads every waiting visit into `yard-samples/<station>/<visit>/` and
removes it from the relay (`--keep` leaves it there).
