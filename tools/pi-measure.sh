#!/bin/bash
# Measure a wasp-host process's CPU, plus the Pi's thermal state, from /proc.
# Usage: pi-measure.sh <pid> <seconds>
pid=$1; secs=${2:-12}
hz=$(getconf CLK_TCK)
read_cpu() { awk '{print $14+$15}' /proc/$1/stat 2>/dev/null; }
temp() { awk '{printf "%.1f", $1/1000}' /sys/class/thermal/thermal_zone0/temp 2>/dev/null; }

t0=$(read_cpu $pid); [ -z "$t0" ] && { echo "process $pid not running"; exit 1; }
temp0=$(temp)
sleep $secs
t1=$(read_cpu $pid); [ -z "$t1" ] && { echo "process $pid exited during sampling"; exit 1; }

cpu=$(echo "scale=1; ($t1-$t0)*100/$hz/$secs" | bc)
threads=$(ls /proc/$pid/task | wc -l)
rss=$(awk '/VmRSS/{print $2}' /proc/$pid/status)
echo "cpu=${cpu}% of one core (of $(nproc) cores)  threads=$threads  rss=$((rss/1024))MB"
echo "temp: ${temp0}C -> $(temp)C"
