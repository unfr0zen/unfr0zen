# unfr0zen.com

Website for [unfr0zen](https://github.com/unfr0zen/unfr0zen). Plain HTML and
CSS, no build step, served by GitHub Pages from `docs/` on `main` with the
custom domain in `CNAME`.

The download page reads the latest release from the GitHub API; `REPO` in
`download.js` names the repository it reads.

Local preview: `python3 -m http.server -d docs` and open http://localhost:8000.
