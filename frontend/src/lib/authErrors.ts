export class AuthApiError extends Error {
  constructor(
    message: string,
    public readonly code: string,
  ) {
    super(message)
    this.name = 'AuthApiError'
  }
}

interface FieldError {
  field: string
  message: string
}

export const REGISTER_ERROR_MAP: Record<string, FieldError> = {
  USERNAME_TAKEN: {
    field: 'username',
    message: 'That username is unavailable. Please choose a different one.',
  },
}
