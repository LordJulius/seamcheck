Call sites seamcheck resolves that CodeQL leaves unresolved, each checked by hand
(`vs_grep.py --verified FILE`: added to CodeQL's callers as truth for every tool).
One `target<TAB>file:line` per line. How they were checked:

- koa.tsv (60): `ctx.set` / `ctx.response.get` reach response.set / response.get through
  `delegate(proto, 'response').method('set')`; `this.set` / `this.get` inside lib/response.js;
  `ctx.inspect()`, `ctx.toJSON()`, `this.request.toJSON()`, `this.response.toJSON()`.
- flask.tsv (277): `app.route` / `app.register_blueprint` / `app.add_url_rule` /
  `app.before_request` / `app.get` where `app` is the conftest fixture (`Flask("flask_test", ...)`)
  or a local `Flask(...)`; `self.app.add_url_rule` in BlueprintSetupState, `self.add_url_rule`
  in Scaffold.route (an App).
- axios.tsv (29): `axios.create` / `axios.mergeConfig` / `axios.toFormData` on the package's
  own default export (`import axios from 'axios'`, `require('axios')`), which lib/axios.js
  builds with `axios.create = ...`, `axios.mergeConfig = mergeConfig`, `axios.toFormData = toFormData`;
  CodeQL links the TypeScript ones to index.d.ts and leaves the CommonJS ones unresolved.
