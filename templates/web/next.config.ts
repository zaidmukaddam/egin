import type { NextConfig } from "next"
import { withEgin } from "egin-ts/next"

const nextConfig: NextConfig = {}

// `next dev` and `next start` also run `egin serve` for the agent in agent/.
export default withEgin(nextConfig)
