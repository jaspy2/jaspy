#!/bin/sh
while true
do
	/usr/bin/python3 discover.py -D -S -n
	sleep 10
done
#exec "$@"
