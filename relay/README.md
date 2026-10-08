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
