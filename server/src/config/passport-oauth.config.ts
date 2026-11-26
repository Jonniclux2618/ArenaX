import passport from 'passport';
import { Strategy as GoogleStrategy, Profile as GoogleProfile } from 'passport-google-oauth20';
import { Strategy as DiscordStrategy, Profile as DiscordProfile } from 'passport-discord';
import { Strategy as TwitchStrategy } from 'passport-twitch-new';
import { resolveSafeOAuthLogin } from '../services/oauth-identity.service';

export const configureOAuthStrategies = (): void => {
    // Google OAuth Strategy — uses safe OAuth resolution (#172) and never
    // silently links to an existing ArenaX account by email without proof.
    if (process.env.GOOGLE_CLIENT_ID && process.env.GOOGLE_CLIENT_SECRET) {
        passport.use(
            new GoogleStrategy(
                {
                    clientID: process.env.GOOGLE_CLIENT_ID,
                    clientSecret: process.env.GOOGLE_CLIENT_SECRET,
                    callbackURL: `${process.env.BACKEND_URL}/api/v1/auth/google/callback`,
                    scope: ['profile', 'email']
                },
                async (_accessToken: string, _refreshToken: string, profile: GoogleProfile, done: any) => {
                    try {
                        const primaryEmail = profile.emails?.[0];
                        const rawVerified = (primaryEmail as { verified?: boolean | string } | undefined)?.verified;
                        const emailVerified = rawVerified === true || rawVerified === 'true';
                        const user = await resolveSafeOAuthLogin({
                            provider: 'GOOGLE',
                            providerId: profile.id,
                            email: primaryEmail?.value || `${profile.id}@google.local`,
                            emailVerified,
                            username: profile.displayName || `google_${profile.id}`,
                        });
                        return done(null, user);
                    } catch (error) {
                        return done(error as Error);
                    }
                }
            )
        );
    }

    // Discord OAuth Strategy — uses safe OAuth resolution (#172).
    if (process.env.DISCORD_CLIENT_ID && process.env.DISCORD_CLIENT_SECRET) {
        passport.use(
            new DiscordStrategy(
                {
                    clientID: process.env.DISCORD_CLIENT_ID,
                    clientSecret: process.env.DISCORD_CLIENT_SECRET,
                    callbackURL: `${process.env.BACKEND_URL}/api/v1/auth/discord/callback`,
                    scope: ['identify', 'email']
                },
                async (_accessToken: string, _refreshToken: string, profile: DiscordProfile, done: any) => {
                    try {
                        const user = await resolveSafeOAuthLogin({
                            provider: 'DISCORD',
                            providerId: profile.id,
                            email: profile.email || `${profile.id}@discord.local`,
                            emailVerified: Boolean((profile as any).verified),
                            username: profile.global_name || profile.username || `discord_${profile.id}`,
                        });
                        return done(null, user);
                    } catch (error) {
                        return done(error as Error);
                    }
                }
            )
        );
    }

    // Twitch OAuth Strategy — uses safe OAuth resolution (#172).
    if (process.env.TWITCH_CLIENT_ID && process.env.TWITCH_CLIENT_SECRET) {
        passport.use(
            new TwitchStrategy(
                {
                    clientID: process.env.TWITCH_CLIENT_ID as string,
                    clientSecret: process.env.TWITCH_CLIENT_SECRET as string,
                    callbackURL: `${process.env.BACKEND_URL}/api/v1/auth/twitch/callback`,
                    scope: ['user:read:email']
                },
                async (_accessToken: string, _refreshToken: string, profile: any, done: any) => {
                    try {
                        const user = await resolveSafeOAuthLogin({
                            provider: 'TWITCH',
                            providerId: String(profile.id),
                            email: profile.email || `${profile.id}@twitch.local`,
                            emailVerified: Boolean(profile.email),
                            username: profile.display_name || profile.login || `twitch_${profile.id}`,
                        });
                        return done(null, user);
                    } catch (error) {
                        return done(error as Error);
                    }
                }
            )
        );
    }
};
