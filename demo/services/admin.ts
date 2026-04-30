import { Service, Handler, Header, Query, Group } from "sky/decorators";
import { HttpError } from "sky/runtime";

@Service({ lifetime: "scoped" })
@Group({
  prefix: "/api/admin",
})
class AdminService {
  @Handler({
    method: "GET",
    path: "/users",
    extract: [Header("x-admin-token"), Query("page")],
  })
  async listAdminUsers(token: string, page?: string) {
    // Simple token check for the demo
    if (!token || token !== "sky-admin-secret") {
      throw new HttpError(401, "Invalid admin token");
    }

    const pageNum = parseInt(page ?? "1", 10);

    return {
      admin: true,
      page: pageNum,
      message: "Admin user listing",
    };
  }

  @Handler({
    method: "GET",
    path: "/stats",
    extract: [Header("x-admin-token")],
  })
  async getStats(token: string) {
    if (!token || token !== "sky-admin-secret") {
      throw new HttpError(401, "Invalid admin token");
    }

    return {
      totalUsers: 42,
      activeToday: 17,
      requestsPerMinute: 1200,
    };
  }
}

export { AdminService };
