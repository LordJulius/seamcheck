"""Findings for machines other than a terminal: SARIF (GitHub code scanning,
most CI dashboards and editors read it) and GitHub Actions annotations (one
`::error file=...,line=...::` line each, shown on the pull request's diff).

Both take the flat finding dicts `check --json` and `gate --json` print:
{rule, severity, file, line, message, why, fix}.
"""
from __future__ import annotations

import json

from . import __version__

_LEVEL = {"error": "error", "warn": "warning", "info": "note"}


def sarif(findings: list[dict], rules: dict[str, dict] | None = None) -> str:
    """A SARIF 2.1.0 log with one run. `rules`: id -> {why, fix} for the
    rule descriptions; a rule a finding names but `rules` lacks is listed bare."""
    rules = dict(rules or {})
    for f in findings:
        rules.setdefault(f["rule"], {"why": f.get("why", ""), "fix": f.get("fix", "")})
    ids = sorted(rules)
    descriptors = []
    for rid in ids:
        meta = rules[rid]
        d = {"id": rid, "name": rid}
        if meta.get("why"):
            d["shortDescription"] = {"text": meta["why"]}
        if meta.get("fix"):
            d["help"] = {"text": meta["fix"]}
        descriptors.append(d)
    results = []
    for f in findings:
        r = {"ruleId": f["rule"], "ruleIndex": ids.index(f["rule"]), "level": _LEVEL.get(f.get("severity"), "error"),
             "message": {"text": f["message"] + (f"  (fix: {f['fix']})" if f.get("fix") else "")}}
        if f.get("file"):
            loc = {"artifactLocation": {"uri": f["file"]}}
            if f.get("line"):
                loc["region"] = {"startLine": int(f["line"])}
            r["locations"] = [{"physicalLocation": loc}]
        results.append(r)
    log = {"$schema": "https://json.schemastore.org/sarif-2.1.0.json", "version": "2.1.0",
           "runs": [{"tool": {"driver": {"name": "seamcheck", "version": __version__,
                                         "informationUri": "https://github.com/LordJulius/seamcheck",
                                         "rules": descriptors}},
                     "results": results}]}
    return json.dumps(log, indent=1)


def _esc_data(s: str) -> str:
    return s.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def _esc_prop(s: str) -> str:
    return _esc_data(s).replace(":", "%3A").replace(",", "%2C")


def github(findings: list[dict]) -> str:
    """GitHub Actions workflow commands: each finding an annotation on its line."""
    out = []
    for f in findings:
        kind = {"warn": "warning", "info": "notice"}.get(f.get("severity"), "error")
        props = []
        if f.get("file"):
            props.append(f"file={_esc_prop(f['file'])}")
            if f.get("line"):
                props.append(f"line={int(f['line'])}")
        props.append(f"title={_esc_prop('seamcheck ' + f['rule'])}")
        msg = f["message"] + (f" - {f['why']}" if f.get("why") else "") + (f" (fix: {f['fix']})" if f.get("fix") else "")
        out.append(f"::{kind} {','.join(props)}::{_esc_data(msg)}")
    return "\n".join(out)
