#!/bin/bash
# n is the seed: $1 when given, else $SEAMCHECK_SEED
n=${1:-$SEAMCHECK_SEED}
echo "SEAMCHECK {\"k\": \"state\", \"t\": 0, \"n\": $n}"
if [ "$n" != 4 ]; then echo 'SEAMCHECK {"k": "end"}'; fi
