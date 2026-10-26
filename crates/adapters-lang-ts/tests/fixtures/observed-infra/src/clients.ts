import { S3Client } from "@aws-sdk/client-s3";
import { MongoClient } from "mongodb";
import { createAuth0Client } from "@auth0/auth0-spa-js";

export const s3 = new S3Client({ region: "eu-west-1" });

export function connect(uri: string) {
  return new MongoClient(uri);
}

export const auth = createAuth0Client({ domain: "tenant.eu.auth0.com", clientId: "x" });
