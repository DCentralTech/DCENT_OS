#!/usr/bin/env python3
"""Compatibility imports for the former AM1-named durability module."""

from durable_file_io import fsync_directory, mkdir_durable

__all__ = ["fsync_directory", "mkdir_durable"]
