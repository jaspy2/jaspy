#!/bin/sh
while true
do
	/usr/bin/python3 discover.py -D -S
	sleep $JASPY_POLLER_DISCOVERY_INTERVAL
done
#exec "$@"
