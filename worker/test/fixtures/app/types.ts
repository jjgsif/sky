export interface CreateUserInput {
    email: string;
    name: string;
    role: "admin" | "member";
}

export interface UserResponse {
    id: string;
    email: string;
    name: string;
    role: string;
    createdAt: string;
}

export interface UpdateUserInput {
    name?: string;
    role?: "admin" | "member";
}