import os

import boto3
from redis import Redis


def bucket():
    return boto3.client("s3", region_name="eu-west-1")


cache = Redis(host=os.environ["REDIS_HOST"])
