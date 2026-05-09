import { Service, Handler, Header, Query, Group, type ExtractContext } from "sky/decorators";
import { HttpError } from "sky/runtime";

const listExtract = {
  token: Header("x-admin-token"),
  page: Query("page"),
} as const;

const statsExtract = {
  token: Header("x-admin-token"),
} as const;

@Service({ lifetime: "scoped" })
@Group({
  prefix: "/api/admin",
})
class AdminService {
  @Handler({
    method: "GET",
    path: "/users",
    extract: listExtract,
  })
  async listAdminUsers({ token, page }: ExtractContext<typeof listExtract>) {
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
    extract: statsExtract,
  })
  async getStats({ token }: ExtractContext<typeof statsExtract>) {
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
