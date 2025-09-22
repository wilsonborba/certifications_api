#!/usr/bin/env bash

sudo systemctl stop clamav-freshclam
sudo systemctl stop clamav-daemon 

sudo systemctl disable clamav-freshclam
sudo systemctl disable clamav-daemon.socket

