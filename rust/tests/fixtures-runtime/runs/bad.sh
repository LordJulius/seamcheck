#!/bin/bash
echo 'SEAMCHECK {"k": "state", "t": 0, "n": 5}'
echo 'SEAMCHECK {"k": "state", "t": 1, "n": 6}' >&2
echo 'FATAL: lost the map' >&2
echo 'SEAMCHECK {"k": "act", "t": 2, "act": "add", "before": 3, "after": 3}'
printf 'SEAMCHECK {"k": "state", "t": 3, "n": 0}\r\nSEAMCHECK {"k": "state", "t": 4, "n": 9}\rlast line without newline'
exit 3
