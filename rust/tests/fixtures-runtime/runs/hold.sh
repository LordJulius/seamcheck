#!/bin/bash
(sleep 30; echo late) &
echo 'SEAMCHECK {"k": "state", "n": 0}'
sleep 60
