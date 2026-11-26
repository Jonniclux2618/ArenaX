-- CreateEnum
CREATE TYPE "WorkspaceRole" AS ENUM ('OWNER', 'ADMIN', 'OPERATOR', 'MODERATOR', 'VIEWER');

-- CreateEnum
CREATE TYPE "WorkspaceMembershipStatus" AS ENUM ('ACTIVE', 'SUSPENDED', 'REVOKED');

-- CreateEnum
CREATE TYPE "OAuthIdentityStatus" AS ENUM ('ACTIVE', 'UNLINKED', 'REVOKED');

-- CreateEnum
CREATE TYPE "OAuthRecoveryStatus" AS ENUM ('PENDING', 'COMPLETED', 'EXPIRED', 'FAILED');

-- CreateTable
CREATE TABLE "OrganizerWorkspace" (
    "id" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "slug" TEXT NOT NULL,
    "ownerId" TEXT NOT NULL,
    "isActive" BOOLEAN NOT NULL DEFAULT true,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "OrganizerWorkspace_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "WorkspaceMembership" (
    "id" TEXT NOT NULL,
    "workspaceId" TEXT NOT NULL,
    "userId" TEXT NOT NULL,
    "role" "WorkspaceRole" NOT NULL DEFAULT 'VIEWER',
    "status" "WorkspaceMembershipStatus" NOT NULL DEFAULT 'ACTIVE',
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "WorkspaceMembership_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "WorkspaceResource" (
    "id" TEXT NOT NULL,
    "workspaceId" TEXT NOT NULL,
    "resourceType" TEXT NOT NULL,
    "resourceId" TEXT NOT NULL,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "WorkspaceResource_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "OAuthIdentity" (
    "id" TEXT NOT NULL,
    "userId" TEXT NOT NULL,
    "provider" TEXT NOT NULL,
    "providerId" TEXT NOT NULL,
    "providerEmail" TEXT,
    "status" "OAuthIdentityStatus" NOT NULL DEFAULT 'ACTIVE',
    "linkedAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "lastVerifiedAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "unlinkedAt" TIMESTAMP(3),
    "revokedAt" TIMESTAMP(3),
    "revokeReason" TEXT,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "OAuthIdentity_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "OAuthLinkChallenge" (
    "id" TEXT NOT NULL,
    "userId" TEXT NOT NULL,
    "provider" TEXT NOT NULL,
    "stateHash" TEXT NOT NULL,
    "nonceHash" TEXT NOT NULL,
    "sessionFingerprint" TEXT NOT NULL,
    "expiresAt" TIMESTAMP(3) NOT NULL,
    "consumedAt" TIMESTAMP(3),
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "OAuthLinkChallenge_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "OAuthRecoveryChallenge" (
    "id" TEXT NOT NULL,
    "userId" TEXT NOT NULL,
    "provider" TEXT,
    "codeHash" TEXT NOT NULL,
    "status" "OAuthRecoveryStatus" NOT NULL DEFAULT 'PENDING',
    "attempts" INTEGER NOT NULL DEFAULT 0,
    "maxAttempts" INTEGER NOT NULL DEFAULT 5,
    "expiresAt" TIMESTAMP(3) NOT NULL,
    "consumedAt" TIMESTAMP(3),
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "OAuthRecoveryChallenge_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
CREATE UNIQUE INDEX "OrganizerWorkspace_slug_key" ON "OrganizerWorkspace"("slug");
CREATE INDEX "OrganizerWorkspace_ownerId_idx" ON "OrganizerWorkspace"("ownerId");
CREATE INDEX "OrganizerWorkspace_isActive_idx" ON "OrganizerWorkspace"("isActive");

CREATE UNIQUE INDEX "WorkspaceMembership_workspaceId_userId_key" ON "WorkspaceMembership"("workspaceId", "userId");
CREATE INDEX "WorkspaceMembership_userId_status_idx" ON "WorkspaceMembership"("userId", "status");
CREATE INDEX "WorkspaceMembership_workspaceId_status_idx" ON "WorkspaceMembership"("workspaceId", "status");

CREATE UNIQUE INDEX "WorkspaceResource_resourceType_resourceId_key" ON "WorkspaceResource"("resourceType", "resourceId");
CREATE INDEX "WorkspaceResource_workspaceId_resourceType_idx" ON "WorkspaceResource"("workspaceId", "resourceType");

CREATE UNIQUE INDEX "OAuthIdentity_provider_providerId_key" ON "OAuthIdentity"("provider", "providerId");
CREATE INDEX "OAuthIdentity_userId_status_idx" ON "OAuthIdentity"("userId", "status");
CREATE INDEX "OAuthIdentity_provider_status_idx" ON "OAuthIdentity"("provider", "status");

CREATE UNIQUE INDEX "OAuthLinkChallenge_stateHash_key" ON "OAuthLinkChallenge"("stateHash");
CREATE INDEX "OAuthLinkChallenge_userId_provider_idx" ON "OAuthLinkChallenge"("userId", "provider");
CREATE INDEX "OAuthLinkChallenge_expiresAt_idx" ON "OAuthLinkChallenge"("expiresAt");

CREATE INDEX "OAuthRecoveryChallenge_userId_status_idx" ON "OAuthRecoveryChallenge"("userId", "status");
CREATE INDEX "OAuthRecoveryChallenge_expiresAt_idx" ON "OAuthRecoveryChallenge"("expiresAt");

-- AddForeignKey
ALTER TABLE "WorkspaceMembership" ADD CONSTRAINT "WorkspaceMembership_workspaceId_fkey" FOREIGN KEY ("workspaceId") REFERENCES "OrganizerWorkspace"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "WorkspaceResource" ADD CONSTRAINT "WorkspaceResource_workspaceId_fkey" FOREIGN KEY ("workspaceId") REFERENCES "OrganizerWorkspace"("id") ON DELETE CASCADE ON UPDATE CASCADE;
