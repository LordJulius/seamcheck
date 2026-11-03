#!/bin/bash
(sleep 30) &
echo 'SEAMCHECK {"k": "state", "n": 0}'
echo 'SEAMCHECK {"k": "end"}'
