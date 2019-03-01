#!/bin/sh
while true
do
	/usr/bin/python3 discover.py -D -S
	sleep 10
done
#exec "$@"
