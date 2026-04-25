struct Point {
    int x;
    int y;
};

int distance_sq(struct Point* p) {
    return p->x * p->x + p->y * p->y;
}

int origin_distance_sq(void) {
    struct Point p = { 3, 4 };
    return distance_sq(&p);
}
